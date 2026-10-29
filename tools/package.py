"""Create a deterministic native package and record its exact identity."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import zipfile

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary",type=Path,required=True)
parser.add_argument("--platform",choices=("linux-x86_64","macos-aarch64"),required=True)
args=parser.parse_args()
root=Path(__file__).resolve().parent.parent
binary=args.binary.read_bytes()
if args.platform=="linux-x86_64":
    assert binary[:6]==b"\x7fELF\x02\x01" and struct.unpack_from("<H",binary,18)[0]==62,"Expected ELF x86_64"
else:
    assert binary[:4]==b"\xcf\xfa\xed\xfe" and struct.unpack_from("<I",binary,4)[0]==0x0100000c,"Expected Mach-O arm64"
assert len(binary)<=32*1024**2,"Executable exceeds the package member limit"
members={name:(root/name).read_bytes() for name in ("manifest.json","icon.svg","LICENSE","NOTICE")}
members["plugin"]=binary
manifest=json.loads(members["manifest.json"])
output=root/"dist"
output.mkdir(exist_ok=True)
target=output/f"deezer-source-{manifest['version']}-{args.platform}.zip"
with zipfile.ZipFile(target,"w",zipfile.ZIP_DEFLATED,compresslevel=9) as archive:
    for name,content in sorted(members.items()):
        info=zipfile.ZipInfo(name,(2026,10,28,0,0,0))
        info.create_system=3
        info.external_attr=(0o100755 if name=="plugin" else 0o100644)<<16
        info.compress_type=zipfile.ZIP_DEFLATED
        archive.writestr(info,content)
record={"file":target.name,"platform":args.platform,"id":manifest["id"],"version":manifest["version"],
    "sha256":hashlib.sha256(target.read_bytes()).hexdigest(),"compressed_bytes":target.stat().st_size,
    "expanded_bytes":sum(map(len,members.values())),"executable_bytes":len(binary),
    "executable_sha256":hashlib.sha256(binary).hexdigest()}
(output/(target.stem+".json")).write_text(json.dumps(record,indent=2)+"\n")
print(json.dumps(record),flush=True)
