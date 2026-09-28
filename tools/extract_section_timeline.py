"""Build a compact, conservative section timeline directly from an export."""
from __future__ import annotations
import argparse, hashlib, json, sys
from pathlib import Path
import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.parquet as pq

if __package__:
    from .atomic_io import aliases, atomic_write_text, sha256_file
    from . import extract_section_observations, section_timeline
else:
    from atomic_io import aliases, atomic_write_text, sha256_file
    import extract_section_observations, section_timeline

sha = sha256_file

def actor_rows(path):
    ordinal=0
    for batch in pq.ParquetFile(path).iter_batches(batch_size=65536, columns=["time_ms","packet_id","channel_index","actor_net_guid","event","class_path"], use_threads=False):
        events=pc.cast(batch.column(batch.schema.get_field_index("event")),pa.string())
        indices=pc.indices_nonzero(pc.is_in(events,value_set=pa.array(["open","close"])))
        for offset,row in zip(indices.to_pylist(),batch.take(indices).to_pylist()):
            row["_ordinal"]=ordinal+offset; yield ordinal+offset,row
        ordinal += batch.num_rows

def extract(export):
    pa.set_cpu_count(1)
    pa.set_io_thread_count(1)
    files=[export/n for n in ("manifest.json","fields.parquet","checkpoint_fields.parquet","net_guids.parquet","actors.parquet")]
    helpers=[Path(__file__).resolve(),Path(__file__).with_name("section_timeline.py"),Path(extract_section_observations.__file__).resolve(),Path(__file__).with_name("atomic_io.py"),Path(__file__).with_name("extract_kill_observations.py")]
    before={p.name:sha(p) for p in files}; impl_before={p.name:sha(p) for p in helpers}
    raw=extract_section_observations.extract(export)
    timeline=section_timeline.build(raw, list(actor_rows(export/"actors.parquet")))
    after={p.name:sha(p) for p in files}; impl_after={p.name:sha(p) for p in helpers}
    if before != after: raise ValueError("input changed during extraction")
    if impl_before != impl_after: raise ValueError("implementation changed during extraction")
    return {"schema_version":1,"kind":"vrfkit_section_timeline","export_id":export.name,"source":str(export.resolve()),"provenance":{"input_sha256_before":before,"input_sha256_after":after,"implementation_sha256_before":impl_before,"implementation_sha256_after":impl_after,"raw_observation_counts":raw["counts"],"replay_build":raw["provenance"]["replay_build"]},**timeline}

def main(argv=None):
    p=argparse.ArgumentParser();p.add_argument("--export",required=True,type=Path);p.add_argument("--out",required=True,type=Path);a=p.parse_args(argv)
    try:
        protected=[x for x in a.export.iterdir() if x.is_file()]+[Path(__file__),Path(__file__).with_name("section_timeline.py"),Path(extract_section_observations.__file__),Path(__file__).with_name("atomic_io.py"),Path(__file__).with_name("extract_kill_observations.py")]
        if aliases(a.out,protected): raise ValueError("output aliases an input or implementation file")
        data=extract(a.export);atomic_write_text(a.out,json.dumps(data,indent=2,sort_keys=True,allow_nan=False)+"\n")
    except (OSError,ValueError,json.JSONDecodeError) as e: print("FAILED: "+str(e),file=sys.stderr);return 1
    print("wrote %s (%d nodes)"%(a.out,data["counts"]["nodes"]));return 0
if __name__=="__main__":raise SystemExit(main())
