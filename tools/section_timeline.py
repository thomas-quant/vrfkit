"""Observed section adjacency; actor lifetime does not prove game lifetime."""
from __future__ import annotations

import collections
import math
import struct

RESET = "MulticastSectionLifeChange"
SIGNS = {"MulticastNotifyDamage_Base": 1, "MulticastNotifyDamage_Point": 1,
         "MulticastNotifyHeal": 1, "MulticastNotifyOverhealDecay": -1}


def _traces(rows):
    actors, channels = collections.defaultdict(list), collections.defaultdict(list)
    for ordinal, row in rows:
        if row.get("event") in ("open", "close"):
            actors[row["actor_net_guid"]].append((ordinal, row))
            channels[row["channel_index"]].append((ordinal, row))
    return actors, channels


def active_at(history, coordinate):
    """Require ordered lifecycle evidence; cross-table same-ms order is unknown.

    Inspect the entire trace for duplicate/regressed clocks before resolving
    a prefix. Sorting first would hide damaged or incomplete lifetime evidence.
    """
    clocks = [(r.get("time_ms"), r.get("packet_id")) for _, r in history]
    if any(t is None or p is None for t, p in clocks):
        return None, "missing_lifecycle_clock"
    if any(b <= a for a, b in zip(clocks, clocks[1:])):
        return None, "lifecycle_clock_duplicate_or_regression"
    if any(t == coordinate["time_ms"] for t, _ in clocks):
        return None, "lifecycle_boundary_same_time"
    active = None
    for ordinal, row in history:
        if row["time_ms"] > coordinate["time_ms"]:
            break
        if row["event"] == "open":
            if active is not None:
                return None, "reopen_without_close"
            active = {**row, "physical_row_ordinal": ordinal}
        elif row["event"] == "close":
            if active is None:
                return None, "close_without_open"
            if any(active[k] != row[k] for k in ("actor_net_guid", "channel_index")):
                return None, "close_identity_mismatch"
            active = None
    if active is None:
        return None, "no_active_actor"
    if any(active[k] != coordinate[k] for k in ("actor_net_guid", "channel_index")):
        return None, "active_identity_mismatch"
    return active, "active"


def _lifetime(actors, channels, ident, active=active_at):
    actor, a_status = active(actors.get(ident["actor_net_guid"], []), ident)
    channel, c_status = active(channels.get(ident["channel_index"], []), ident)
    matched = actor is not None and channel is not None and actor == channel
    return {"status": "active" if matched else "unresolved", "actor_status": a_status,
            "channel_status": c_status, "actor_open": actor, "channel_open": channel}


def _token_comparison(prior, current, previous_route, current_route):
    """Compare identical raw roles only; never assign a shared respawn domain."""
    result = []
    for role in sorted(set(prior) | set(current)):
        left, right = prior.get(role, []), current.get(role, [])
        same_domain = previous_route == current_route
        known = (same_domain and len(left) == len(right) == 1 and all(
            r.get("raw_bits_hex") is not None and r.get("bit_count") is not None for r in left + right))
        equal = ((left[0]["bit_count"], left[0]["raw_bits_hex"]) ==
                 (right[0]["bit_count"], right[0]["raw_bits_hex"])) if known else None
        result.append({"role": role, "previous_route": previous_route, "current_route": current_route,
                       "previous": left, "current": right, "raw_equal": equal,
                       "same_route_domain": same_domain})
    return result


def _arithmetic(prior, section, route):
    result = {"expected_sign": SIGNS.get(route), "predicted_result": None,
              "matches": None, "status": "no_predecessor"}
    if route == RESET:
        result["status"] = "absolute_reset_no_delta_edge"
    elif prior is not None and route in SIGNS:
        value = prior["life_result"] + SIGNS[route] * section["delta_life"]
        try:
            prediction = struct.unpack("<f", struct.pack("<f", value))[0]
        except OverflowError:
            prediction = math.inf
        if math.isfinite(prediction):
            result.update(predicted_result=prediction, matches=prediction == section["life_result"],
                          status="observed_arithmetic_only")
        else:
            result["status"] = "f32_prediction_overflow"
    return result


def build(raw, actor_rows):
    """Retain states and barriers; adjacency eligibility is not effective HP."""
    actors, channels = _traces(actor_rows)
    observations = sorted(raw["observations"], key=lambda o: min(
        r["physical_row_ordinal"] for r in o["source_rows"]))
    scope_epochs, actor_epochs = collections.Counter(), collections.Counter()
    last_objects, last_clocks, previous = {}, {}, {}
    time_counts = collections.Counter()
    for obs in observations:
        ident = obs["identity"]
        if obs["section_state"]["status"] == "validated_array" and not obs.get("schema_errors"):
            for section in obs["section_state"]["sections"]:
                time_counts[(ident["actor_net_guid"], ident["object_net_guid"],
                             section["changed_component_ref"], ident["time_ms"])] += 1
    nodes, barriers, warnings = [], [], []
    for obs in observations:
        ident, state = obs["identity"], obs["section_state"]
        actor_id, object_id = ident["actor_net_guid"], ident["object_net_guid"]
        scope = actor_id, object_id
        clock = ident["time_ms"], ident["packet_id"]
        object_changed = actor_id in last_objects and last_objects[actor_id] != object_id
        clock_regressed = actor_id in last_clocks and clock < last_clocks[actor_id]
        if object_changed or clock_regressed:
            actor_epochs[actor_id] += 1
        last_objects[actor_id], last_clocks[actor_id] = object_id, clock
        ordinals = [r["physical_row_ordinal"] for r in obs["source_rows"]]
        origin = {"physical_row_ordinals": ordinals, "route": obs["route"], "coordinate": ident}
        if state["status"] != "validated_array" or obs.get("schema_errors"):
            scope_epochs[scope] += 1
            barriers.append({"kind": "schema_invalid" if obs.get("schema_errors") else state["status"],
                "actor_object": list(scope), "epoch_after": scope_epochs[scope], "origin": origin,
                "schema_errors": obs.get("schema_errors", []),
                "raw_lifecycle_tokens": obs.get("raw_lifecycle_tokens", {})})
            continue
        reset = obs["route"] == RESET
        raw_ambiguities = [r for r in obs.get("ambiguity_reasons", [])
                           if r != "scalar_delta_relation_mismatch"]
        if reset or raw_ambiguities:
            scope_epochs[scope] += 1
        relation = state.get("relation")
        if isinstance(relation, dict) and relation.get("matches") is False:
            warnings.append({"kind": "scalar_relation_mismatch", "origin": origin, "relation": relation})
        lifetime = _lifetime(actors, channels, ident)
        for section in state["sections"]:
            key = (*scope, section["changed_component_ref"])
            prior = previous.get(key)
            reasons = []
            tied = time_counts[(*key, ident["time_ms"])] > 1
            if tied:
                reasons.append("same_time_tie")
            if reset:
                reasons.append("reset_state")
            if raw_ambiguities:
                reasons.append("raw_observation_ambiguous")
            if clock_regressed:
                reasons.append("clock_regression")
            if lifetime["status"] != "active":
                reasons.append("lifecycle_unresolved")
            comparisons = []
            if prior is None:
                reasons.append("no_prior_node")
            else:
                if prior["epoch"] != scope_epochs[scope] or prior["actor_epoch"] != actor_epochs[actor_id]:
                    reasons.append("persistent_barrier_epoch")
                if prior["same_time_tie"]:
                    reasons.append("prior_tie_censor")
                if prior["clock_regression"]:
                    reasons.append("prior_clock_regression")
                if prior["raw_observation_ambiguities"]:
                    reasons.append("prior_raw_observation_ambiguous")
                if prior["actor_lifecycle"]["status"] != "active":
                    reasons.append("prior_lifecycle_unresolved")
                elif lifetime["status"] == "active" and prior["actor_lifecycle"] != lifetime:
                    reasons.append("actor_channel_instance_changed")
                p_coord = prior["origin"]["coordinate"]
                if p_coord["channel_index"] != ident["channel_index"]:
                    reasons.append("channel_changed")
                if clock <= (p_coord["time_ms"], p_coord["packet_id"]):
                    reasons.append("nonincreasing_clock")
                if prior["state_key"]["changed_component_path"] != section.get("changed_component_path"):
                    reasons.append("section_path_changed")
                comparisons = _token_comparison(prior["raw_lifecycle_tokens"], obs.get("raw_lifecycle_tokens", {}),
                                                prior["origin"]["route"], obs["route"])
                if any(c["raw_equal"] is False for c in comparisons):
                    reasons.append("opaque_token_changed")
                if any(c["raw_equal"] is None for c in comparisons):
                    reasons.append("opaque_token_gap")
            node = {"node_id": [min(ordinals), section["index"]],
                "state_key": {"actor_net_guid": actor_id, "object_net_guid": object_id,
                    "changed_component_ref": key[2], "changed_component_path": section.get("changed_component_path")},
                "epoch": scope_epochs[scope], "actor_epoch": actor_epochs[actor_id], "origin": origin,
                "life_result": section["life_result"], "delta_life": section["delta_life"],
                "alive_after_change": section["alive_after_change"], "same_time_tie": tied,
                "raw_observation_ambiguities": raw_ambiguities,
                "clock_regression": clock_regressed, "previous_node_id": prior["node_id"] if prior else None,
                "observed_before_after_difference": section["life_result"] - prior["life_result"] if prior else None,
                "continuity": {"eligible": not reasons, "reasons": sorted(set(reasons)),
                    "scope": "ordered_observation_comparison_only", "game_life": "unproved", "component_life": "unproved"},
                "actor_lifecycle": lifetime, "raw_lifecycle_tokens": obs.get("raw_lifecycle_tokens", {}),
                "raw_token_comparisons": comparisons, "scalar_relation": relation,
                "route_arithmetic": _arithmetic(prior, section, obs["route"])}
            nodes.append(node)
            previous[key] = node
    return {"nodes": nodes, "barriers": barriers, "scalar_warnings": warnings,
        "counts": {"nodes": len(nodes), "barriers": len(barriers), "scalar_warnings": len(warnings),
            "continuity_eligible": sum(n["continuity"]["eligible"] for n in nodes),
            "continuity_ineligible": sum(not n["continuity"]["eligible"] for n in nodes)}}
