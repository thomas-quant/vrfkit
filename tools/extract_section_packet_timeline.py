"""Build strict and packet-resolved section timelines directly from an export."""
from __future__ import annotations
import argparse,hashlib,json,sys
from pathlib import Path
import pyarrow as pa
if __package__:
 from .atomic_io import aliases, atomic_write_text, sha256_file as sha
 from . import extract_section_timeline,section_packet_timeline
else:
 from atomic_io import aliases, atomic_write_text, sha256_file as sha
 import extract_section_timeline,section_packet_timeline

def helpers():
 names=[Path(__file__),Path(section_packet_timeline.__file__),Path(extract_section_timeline.__file__),Path(extract_section_timeline.section_timeline.__file__),Path(extract_section_timeline.extract_section_observations.__file__),Path(extract_section_timeline.__file__).with_name("atomic_io.py"),Path(extract_section_timeline.__file__).with_name("extract_kill_observations.py")]
 return [p.resolve() for p in names]
def extract(export):
 pa.set_cpu_count(1);pa.set_io_thread_count(1)
 inputs=[export/n for n in ("manifest.json","fields.parquet","checkpoint_fields.parquet","net_guids.parquet","actors.parquet")];impl=helpers();before={str(p.resolve()):sha(p) for p in inputs};ib={str(p):sha(p) for p in impl}
 strict=extract_section_timeline.extract(export);strict_projection_sha256=hashlib.sha256(json.dumps(strict,sort_keys=True,allow_nan=False,separators=(",",":")).encode()).hexdigest();rows=list(extract_section_timeline.actor_rows(export/"actors.parquet"));result=section_packet_timeline.build(strict,rows,population="main")
 after={str(p.resolve()):sha(p) for p in inputs};ia={str(p):sha(p) for p in impl}
 if before!=after:raise ValueError("input changed during extraction")
 if ib!=ia:raise ValueError("implementation changed during extraction")
 result["packet_provenance"]={"strict_projection_sha256":strict_projection_sha256,"input_sha256_before":before,"input_sha256_after":after,"implementation_sha256_before":ib,"implementation_sha256_after":ia,"population":"main_only"};return result
def main(argv=None):
 p=argparse.ArgumentParser();p.add_argument("--export",required=True,type=Path);p.add_argument("--out",required=True,type=Path);a=p.parse_args(argv)
 try:
  protected=[x.resolve() for x in a.export.iterdir() if x.is_file()]+helpers()
  if aliases(a.out,protected):raise ValueError("output aliases an input or implementation file")
  data=extract(a.export);atomic_write_text(a.out,json.dumps(data,indent=2,sort_keys=True,allow_nan=False)+"\n")
 except (OSError,ValueError,json.JSONDecodeError) as e:print("FAILED: "+str(e),file=sys.stderr);return 1
 print("wrote %s (%d packet-eligible, %d resolved)"%(a.out,data["packet_counts"]["eligible"],data["packet_counts"]["resolved_from_strict_ineligible"]));return 0
if __name__=="__main__":raise SystemExit(main())
