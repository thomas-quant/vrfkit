"""Join declared character-death events to component-local KillData evidence.

Event identity comes from active character PlayerState references. Time is only
a bounded corroboration; ambiguous matches are never resolved by greedy order.
"""
from __future__ import annotations

from collections import Counter, defaultdict
import argparse
import json
import math
from pathlib import Path
import struct
import sys

import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

if __package__:
    from . import extract_kill_observations as observation_extractor
    from .atomic_io import atomic_write_text, sha256_file
    from .extract_kill_observations import InputError, exact_ref
else:
    import extract_kill_observations as observation_extractor
    from atomic_io import atomic_write_text, sha256_file
    from extract_kill_observations import InputError, exact_ref

MAX_REPLICATION_LAG_MS = 50
#: This file and the modules it runs, hashed as provenance and refused as --out.
SOURCE_NAMES = ('extract_kill_ledger.py','kill_state.py','extract_kill_observations.py','atomic_io.py')
DEATH_NAME = 'EReplayEventGroup::CharacterDeath'
IDENTITY_STATUSES = (
    'resolved', 'absent_reference', 'null_reference', 'lifecycle_time_regression',
    'event_time_lifecycle_boundary', 'no_prior_actor_open', 'ambiguous_actor_lifecycle',
    'actor_closed', 'non_character_actor', 'event_time_mapping_boundary',
    'no_prior_player_state', 'ambiguous_player_state', 'untyped_player_state',
    'null_player_state', 'unvalidated_payload',
)


class ActorIdentityIndex:
    """Resolve top-level PlayerState within the active actor lifetime."""

    def __init__(self, actors, player_states):
        self.lifecycle = defaultdict(list)
        self.properties = defaultdict(list)
        self.invalid_order = set()
        for ordinal, row in enumerate(actors):
            guid = row['actor_net_guid']
            if row['event'] not in ('open', 'close', 'dormant'):
                raise InputError('unknown actor lifecycle event')
            if row['event'] == 'dormant':
                continue
            previous = self.lifecycle[guid]
            if previous and (row['time_ms'], row['packet_id']) < previous[-1]['coordinate']:
                self.invalid_order.add(guid)
            previous.append({'coordinate': (row['time_ms'], row['packet_id']),
                             'event': row['event'], 'class_path': row['class_path'],
                             'source_row': ordinal})
        for ordinal, row in player_states:
            if row['field_name'] != 'PlayerState' or row['object_net_guid'] is not None:
                continue
            value = row['value_i64']
            if value is not None and (type(value) is not int or not 0 <= value <= 0xffffffff):
                raise InputError('PlayerState column is not a u32 reference')
            self.properties[row['actor_net_guid']].append(dict(row, source_row=ordinal))

    def resolve(self, guid, timestamp):
        result = {'pawn_ref': guid, 'player_state_ref': None}

        def unresolved(reason):
            return dict(result, status=reason)

        if guid is None:
            return unresolved('absent_reference')
        if guid == 0:
            return unresolved('null_reference')
        if guid in self.invalid_order:
            return unresolved('lifecycle_time_regression')
        lifetime = self.lifecycle.get(guid, [])
        if any(x['coordinate'][0] == timestamp for x in lifetime):
            return unresolved('event_time_lifecycle_boundary')
        prior = [x for x in lifetime if x['coordinate'][0] < timestamp]
        if not prior:
            return unresolved('no_prior_actor_open')
        last = max(x['coordinate'] for x in prior)
        boundaries = [x for x in prior if x['coordinate'] == last]
        if len(boundaries) != 1:
            return unresolved('ambiguous_actor_lifecycle')
        opened = boundaries[0]
        if opened['event'] != 'open':
            return unresolved('actor_closed')
        result.update(actor_class=opened['class_path'], actor_open_row=opened['source_row'])
        if not (opened['class_path'] or '').startswith('/Game/Characters/'):
            return unresolved('non_character_actor')
        samples = [x for x in self.properties.get(guid, [])
                   if x['group_path'] == opened['class_path']
                   and (x['time_ms'], x['packet_id']) >= opened['coordinate']
                   and x['time_ms'] <= timestamp]
        if any(x['time_ms'] == timestamp for x in samples):
            return unresolved('event_time_mapping_boundary')
        if not samples:
            return unresolved('no_prior_player_state')
        latest = max((x['time_ms'], x['packet_id']) for x in samples)
        latest_rows = [x for x in samples if (x['time_ms'], x['packet_id']) == latest]
        values = {x['value_i64'] for x in latest_rows}
        if len(values) != 1:
            return unresolved('ambiguous_player_state')
        value = next(iter(values))
        result.update(mapping_field_rows=[x['source_row'] for x in latest_rows],
                      mapping_time_ms=latest[0], mapping_packet_id=latest[1])
        if value is None:
            return unresolved('untyped_player_state')
        if type(value) is not int or not 0 <= value <= 0xffffffff:
            raise InputError('PlayerState is not a u32 reference')
        for row in latest_rows:
            if any(row[k] is not None for k in ('value_f64', 'value_bool', 'value_str')):
                raise InputError('PlayerState has conflicting typed columns')
            raw, bits = row['raw_bits'], row['bit_count']
            if raw is None or len(raw) != (bits+7)//8 or exact_ref(raw, bits) != value:
                raise InputError('PlayerState typed value disagrees with raw reference')
        if value == 0:
            return unresolved('null_player_state')
        return dict(result, player_state_ref=value, status='resolved')


def validate_event_payload(row, word_count, expected_tag, expected_name):
    """Check the whole measured event payload, independently of its columns."""
    raw = row['raw_payload']
    string_offset = 8 + 4*word_count
    if raw is None or len(raw) != row['payload_size'] or len(raw) < string_offset+4:
        return 'payload_size'
    tag = struct.unpack_from('<I', raw)[0]
    length = struct.unpack_from('<i', raw, string_offset-4)[0]
    if tag != expected_tag or type(row['payload_tag']) is not int or row['payload_tag'] != tag:
        return 'payload_tag'
    for index in range(2):
        value = row['word'+str(index)]
        if index < word_count:
            if type(value) is not int or value != struct.unpack_from('<I', raw, 4+4*index)[0]:
                return 'payload_words'
        elif value is not None:
            return 'payload_words'
    width = length if length > 0 else -2*length
    if not 0 < width <= 65536 or len(raw) != string_offset+width+4:
        return 'payload_string_length'
    text = raw[string_offset:string_offset+width]
    terminator = b'\x00' if length > 0 else b'\x00\x00'
    if not text.endswith(terminator):
        return 'payload_string_terminator'
    try:
        name = text[:-len(terminator)].decode('utf-8' if length > 0 else 'utf-16-le')
    except UnicodeDecodeError:
        return 'payload_string_encoding'
    if name != expected_name or row['payload_name'] != name:
        return 'payload_name'
    seconds = struct.unpack('<f', raw[-4:])[0]
    if not math.isfinite(seconds) or row['payload_seconds'] is None:
        return 'payload_seconds'
    if (type(row['payload_seconds']) is not float
        or struct.pack('<d', row['payload_seconds']) != struct.pack('<d', float(seconds))):
        return 'payload_seconds_column'
    if type(row['time1']) is not int or row['time1'] < 0 or abs(seconds*1000-row['time1']) > 1.001:
        return 'payload_time'
    return None


def validate_death_payload(row):
    return validate_event_payload(row, 2, 8, DEATH_NAME)


def resolve_round(rounds, timestamp):
    """Use one strictly prior validated round start; never infer event order."""
    if type(timestamp) is not int:
        return {'status':'invalid_event_time', 'round_number':None}
    if any(type(row['time1']) is not int for row in rounds):
        return {'status':'unorderable_round', 'round_number':None}
    if any(row['time1'] == timestamp for row in rounds):
        return {'status':'event_time_round_boundary', 'round_number':None}
    prior = [row for row in rounds if row['time1'] < timestamp]
    if not prior:
        return {'status':'no_prior_round', 'round_number':None}
    latest_time = max(row['time1'] for row in prior)
    latest = [row for row in prior if row['time1'] == latest_time]
    if len(latest) != 1:
        return {'status':'ambiguous_round', 'round_number':None}
    row = latest[0]
    return {'status':'resolved' if row['payload_issue'] is None else 'unvalidated_round',
            'round_number':row['word0'] if row['payload_issue'] is None else None,
            'round_event_row_ordinal':row['event_row_ordinal']}


def match_deaths(deaths, complete_main):
    """Require exactly one candidate on each side, without consuming greedily."""
    by_pair = defaultdict(list)
    for index, death in enumerate(deaths):
        killer, victim = death['killer_identity'], death['victim_identity']
        if (death['payload_issue'] is None and killer['status'] == victim['status'] == 'resolved'
            and death['round_identity']['status'] == 'resolved'):
            by_pair[(killer['player_state_ref'], victim['player_state_ref'],
                     death['round_identity']['round_number'])].append(index)
    candidates, reverse = [], defaultdict(list)
    for index, observation in enumerate(complete_main):
        pair = (observation['actor_net_guid'], observation['members']['victim_ref'],
                observation['members']['round_number'])
        matches = [i for i in by_pair.get(pair, [])
                   if 0 <= observation['time_ms']-deaths[i]['time1'] <= MAX_REPLICATION_LAG_MS]
        candidates.append(matches)
        for i in matches:
            reverse[i].append(index)
    return [(index, options[0]) for index, options in enumerate(candidates)
            if len(options) == 1 and len(reverse[options[0]]) == 1], candidates, dict(reverse)


file_sha = sha256_file

def player_state_rows(path):
    """Preserve physical field ordinals while selecting top-level properties."""
    columns = ['actor_net_guid','object_net_guid','group_path','field_name','time_ms','packet_id',
               'value_i64','value_f64','value_bool','value_str','raw_bits','bit_count']
    offset = 0
    result = []
    for batch in pq.ParquetFile(path).iter_batches(batch_size=65536,columns=columns,use_threads=False):
        selected = pc.fill_null(pc.and_(pc.equal(pc.cast(batch['field_name'],pa.string()),'PlayerState'),
                                        pc.is_null(batch['object_net_guid'])),False)
        indices = pc.indices_nonzero(selected)
        rows = batch.take(indices).to_pylist()
        result.extend((offset+index,row) for index,row in zip(indices.to_pylist(),rows))
        offset += batch.num_rows
    return result


def extract(export, observations_path=None):
    """Return all death events, validated state, and explicit unmatched records."""
    if __package__:
        from . import kill_state
    else:
        import kill_state
    export = Path(export)
    sources = {name:file_sha(Path(__file__).parent/name) for name in SOURCE_NAMES}
    parquet_names = ('fields','checkpoint_fields','actors','net_guids','checkpoint_actors',
                     'checkpoint_net_guids','checkpoint_export_groups','checkpoint_export_fields','events')
    inputs = {name+'.parquet':file_sha(export/(name+'.parquet')) for name in parquet_names}
    manifest_hash = file_sha(export/'manifest.json')
    manifest = json.loads((export/'manifest.json').read_text(encoding='utf-8'))
    cache_hash = None
    if observations_path is None:
        observations = observation_extractor.extract(export)
    else:
        observations_path = Path(observations_path)
        cache_hash = file_sha(observations_path)
        observations = json.loads(observations_path.read_text(encoding='utf-8'))
    expected_inputs = {k:v for k,v in inputs.items() if k != 'events.parquet'}
    provenance = observations['provenance']
    if (provenance['export_id'] != export.name or provenance['replay_build'] != manifest['replay_build']
        or provenance['manifest_sha256'] != manifest_hash or provenance['input_sha256'] != expected_inputs
        or provenance['extractor_sha256'] != sources['extract_kill_observations.py']):
        raise InputError('observation provenance does not match this export and extractor')
    if observations_path is not None:
        # Receipts identify inputs; they do not prove that cached values were
        # derived from those inputs. Check the document against fresh extraction.
        fresh = observation_extractor.extract(export)
        canonical = lambda value: json.dumps(value, sort_keys=True, ensure_ascii=True, allow_nan=False)
        if canonical(observations) != canonical(fresh):
            raise InputError('observation cache content differs from source export')
    projection = kill_state.project_kill_state(observations)
    actor_rows = pq.read_table(export/'actors.parquet',use_threads=False).to_pylist()
    identity = ActorIdentityIndex(actor_rows,player_state_rows(export/'fields.parquet'))
    deaths = []
    identity_counts = Counter({side+':'+status: 0 for side in ('killer', 'victim')
                               for status in IDENTITY_STATUSES})
    event_rows = pq.read_table(export/'events.parquet',use_threads=False).to_pylist()
    rounds = []
    for ordinal, row in enumerate(event_rows):
        if row['group'] == 'roundStarted':
            record = dict(row, event_row_ordinal=ordinal,
                          payload_issue=validate_event_payload(row, 1, 2, 'EReplayEventGroup::RoundStart'))
            raw = record.pop('raw_payload')
            record['raw_payload_hex'] = raw.hex() if raw is not None else None
            rounds.append(record)
    for ordinal,row in enumerate(event_rows):
        if row['group'] != 'characterDeath':
            continue
        issue = validate_death_payload(row)
        killer = identity.resolve(row['word0'],row['time1']) if issue is None else {'status':'unvalidated_payload','pawn_ref':row['word0'],'player_state_ref':None}
        victim = identity.resolve(row['word1'],row['time1']) if issue is None else {'status':'unvalidated_payload','pawn_ref':row['word1'],'player_state_ref':None}
        identity_counts['killer:'+killer['status']] += 1
        identity_counts['victim:'+victim['status']] += 1
        same = killer['player_state_ref'] == victim['player_state_ref']
        relation = ('same_player_state' if same else 'distinct_player_states') if killer['status'] == victim['status'] == 'resolved' else 'unresolved'
        record = dict(row)
        raw_payload = record.pop('raw_payload')
        record['raw_payload_hex'] = raw_payload.hex() if raw_payload is not None else None
        record.update(event_row_ordinal=ordinal,payload_issue=issue,killer_identity=killer,
                      victim_identity=victim,identity_relation=relation,
                      round_identity=resolve_round(rounds,row['time1']))
        deaths.append(record)
    complete_indices = [i for i,x in enumerate(observations['observations'])
                        if x['source_table']=='fields' and x['members_complete']]
    complete = [observations['observations'][i] for i in complete_indices]
    pairs,candidates,reverse = match_deaths(deaths,complete)
    event_to_observation = {event:index for index,event in pairs}
    observation_to_event = dict(pairs)
    entity_indices = {(x['key']['object_net_guid'],x['key']['element_index']):i
                      for i,x in enumerate(projection['entities'])}
    for index,death in enumerate(deaths):
        options = reverse.get(index,[])
        if index in event_to_observation:
            observation_index = event_to_observation[index]
            observation = complete[observation_index]
            death['killdata_join'] = {
                'status':'matched','observation_source_index':complete_indices[observation_index],
                'entity_index':entity_indices[(observation['object_net_guid'],observation['element_index'])],
                'replication_lag_ms':observation['time_ms']-death['time1'],
                'candidate_observation_indices':[complete_indices[i] for i in options],
            }
        else:
            death['killdata_join'] = {'status':'no_candidate' if not options else 'ambiguous',
                                     'candidate_observation_indices':[complete_indices[i] for i in options]}
    unmatched = [dict(observation_source_index=complete_indices[i],
                      entity_index=entity_indices[(x['object_net_guid'],x['element_index'])],
                      status='no_candidate' if not candidates[i] else 'ambiguous',
                      candidate_event_row_ordinals=[deaths[j]['event_row_ordinal'] for j in candidates[i]])
                 for i,x in enumerate(complete) if i not in observation_to_event]
    for item in unmatched:
        observation=observations['observations'][item['observation_source_index']]
        item['different_killer_same_victim_context'] = [
            {'event_row_ordinal':death['event_row_ordinal'],
             'event_killer_player_state_ref':death['killer_identity']['player_state_ref'],
             'replication_lag_ms':observation['time_ms']-death['time1']}
            for death in deaths
            if death['payload_issue'] is None
            and death['killer_identity']['status'] == death['victim_identity']['status'] == 'resolved'
            and death['round_identity']['status'] == 'resolved'
            and death['round_identity']['round_number'] == observation['members']['round_number']
            and death['victim_identity']['player_state_ref'] == observation['members']['victim_ref']
            and death['killer_identity']['player_state_ref'] != observation['actor_net_guid']
            and 0 <= observation['time_ms']-death['time1'] <= MAX_REPLICATION_LAG_MS
        ]
    if inputs != {name:file_sha(export/name) for name in inputs} or manifest_hash != file_sha(export/'manifest.json'):
        raise InputError('source export changed during ledger extraction')
    if cache_hash is not None and cache_hash != file_sha(observations_path):
        raise InputError('observation cache changed during extraction')
    if sources != {name:file_sha(Path(__file__).parent/name) for name in sources}:
        raise InputError('extractor source changed during execution')
    return {
        'schema_version':1,'kind':'vrfkit_character_death_ledger',
        'provenance':{'export_id':export.name,'replay_build':manifest['replay_build'],
                      'input_sha256':inputs,'manifest_sha256':manifest_hash,'source_sha256':sources,
                      'observation_cache_sha256':cache_hash},
        'join_contract':{'identity':'active character PlayerState at prior time',
                         'round':'matching KillData round_number and strictly prior validated roundStarted word0',
                         'lag_ms_min':0,'lag_ms_max':MAX_REPLICATION_LAG_MS,'mutual_unique_required':True},
        'counts':{'character_death_events':len(deaths),'validated_payloads':sum(x['payload_issue'] is None for x in deaths),
                  'matched_pairs':len(pairs),'unmatched_events':len(deaths)-len(pairs),
                  'complete_main_observations':len(complete),'unmatched_main_observations':len(unmatched),
                  'ambiguous_events':sum(x['killdata_join']['status']=='ambiguous' for x in deaths),
                  'ambiguous_main_observations':sum(x['status']=='ambiguous' for x in unmatched),
                  'unmatched_with_different_killer_context':sum(bool(x['different_killer_same_victim_context']) for x in unmatched),
                  'round_start_events':len(rounds),
                  'validated_round_payloads':sum(x['payload_issue'] is None for x in rounds),
                  'unresolved_death_rounds':sum(x['round_identity']['status']!='resolved' for x in deaths),
                  'identity_status':dict(identity_counts),'state':projection['counts']},
        'death_events':deaths,'round_start_events':rounds,'unmatched_main_observations':unmatched,
        'state_projection':projection,'source_observations':observations,
    }


def main(argv=None):
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--export',type=Path,required=True)
    parser.add_argument('--observations',type=Path,help='Optional observation document to verify against fresh extraction')
    parser.add_argument('--out',type=Path,required=True)
    args=parser.parse_args(argv)
    pa.set_cpu_count(1)
    pa.set_io_thread_count(1)
    try:
        observation_extractor.reject_overwrite(args.export,args.out)
        protected=[Path(__file__).with_name(name) for name in SOURCE_NAMES]
        if args.observations is not None:
            protected.append(args.observations)
        if args.out.resolve() in {p.resolve() for p in protected}:
            raise InputError('output aliases source code or observation input')
        result=extract(args.export,args.observations)
        atomic_write_text(args.out,json.dumps(result,ensure_ascii=True,separators=(',',':'),allow_nan=False)+'\n')
    except (OSError,ValueError,KeyError,TypeError) as exc:
        print(f'FAILED: {exc}',file=sys.stderr)
        return 1
    print(f"wrote {args.out}: {result['counts']['character_death_events']} character-death events, {result['counts']['matched_pairs']} matched observations")
    return 0


if __name__=='__main__':
    raise SystemExit(main())
