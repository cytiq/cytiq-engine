#!/usr/bin/env python3
"""Fixed, labelled 100k/1M workloads; preparation is outside timed children."""
import argparse
import json
from pathlib import Path
import subprocess
import sys

ROOT=Path(__file__).resolve().parents[2]
sys.path.insert(0,str(ROOT/"parity/gen"))
from generate import gen_autospectral


def run(binary,*args): subprocess.run([str(binary),*map(str,args)],check=True,stdout=subprocess.DEVNULL)


def prepare(output,helper,binary,events,aurora=None,accept=()):
    output=output.resolve(); output.mkdir(parents=True,exist_ok=False)
    cases=[]
    channels=[f"FL{i}-A" for i in range(1,27)]
    for n in events:
        root=output/str(n); root.mkdir()
        samples=[]
        for i in range(2):
            raw=root/f"control{i}.fcs"
            compensated=root/f"control{i}.comp.fcs"
            transformed=root/f"control{i}.transformed.fcs"
            run(helper,"generate","--output",raw,"--events",n,"--seed",42+i,"--labels")
            run(binary,"compensate","--input",raw,"--output",compensated)
            run(binary,"transform","--input",compensated,"--output",transformed,"--method","arcsinh","--channels",",".join(channels))
            samples.append({"id":f"s{i}","batch":f"b{i}","file":str(transformed),"role":"train"})
        manifest={"schema_version":1,"panel":{"id":"performance26","markers":[{"marker":c,"channel":c} for c in channels]},"mode":"aggregate","clustering_markers":channels,"normalization_markers":channels,"samples":samples,"som":{"xdim":5,"ydim":5,"metaclusters":5,"scale":False,"seed":42},"training_events":3000,"min_cluster_events":100,"sparse_policy":"identity"}
        manifest_path=root/"cytonorm.json"; manifest_path.write_text(json.dumps(manifest,indent=2)+"\n")
        # A fixed model, trained outside the timed children, for the apply case.
        model=root/"cytonorm-model"
        run(binary,"cytonorm","train","--manifest",manifest_path,"--output",model)
        model_inputs=[str(p) for p in sorted(model.rglob("*")) if p.is_file() and not p.name.endswith(".lock")]
        cases += [{"name":f"flowsom_{n}","inputs":[str(root/"control0.fcs")],"arguments":["flowsom","--input",str(root/"control0.fcs"),"--output","{output}/assignments.csv","--channels",",".join(channels),"--training-events","10000","--xdim","5","--ydim","5"],"acceptance":False},
                  {"name":f"peacoqc_{n}","inputs":[str(root/"control0.fcs")],"arguments":["peacoqc","--input",str(root/"control0.fcs"),"--output","{output}/cleaned.fcs","--channels",",".join(channels)],"acceptance":False},
                  {"name":f"cytonorm_apply_{n}","inputs":[samples[1]["file"],*model_inputs],"arguments":["cytonorm","apply","--input",samples[1]["file"],"--model",str(model),"--batch","b1","--output","{output}/normalized.fcs"],"acceptance":False},
                  {"name":f"cytonorm_{n}","inputs":[str(manifest_path),*[s["file"] for s in samples]],"arguments":["cytonorm","train","--manifest",str(manifest_path),"--output","{output}/model"],"acceptance":False}]
        spectral=root/"spectral"; spectral.mkdir()
        gen_autospectral(spectral,n_events=5000,n_sample=n)
        cases.append({"name":f"autospectral_{n}","inputs":[str(p) for p in spectral.glob("*.fcs")]+[str(spectral/"controls.csv"),str(spectral/"samples.csv")],"arguments":["autospectral","--controls",str(spectral/"controls.csv"),"--samples",str(spectral/"samples.csv"),"--input-dir",str(spectral),"--output","{output}/unmixed"],"acceptance":False})
    if aurora is not None:
        aurora=aurora.resolve()
        inputs=[aurora/"control_file.csv",aurora/"samples.csv",*sorted((aurora/"controls").glob("*.fcs")),*sorted((aurora/"sample").glob("*.fcs"))]
        if len(inputs) < 4: raise ValueError("required real Aurora inputs unavailable")
        cases.append({"name":"autospectral_aurora64","inputs":list(map(str,inputs)),"arguments":["autospectral","--controls",str(aurora/"control_file.csv"),"--samples",str(aurora/"samples.csv"),"--input-dir",str(aurora/"controls"),"--output","{output}/unmixed"],"acceptance":False})
    # Acceptance is a claim about a specific optimization, so the run names
    # the cases it optimizes; every other case is reported as a diagnostic.
    names={case["name"] for case in cases}
    unknown=set(accept)-names
    if unknown: raise ValueError(f"unknown acceptance cases: {sorted(unknown)}")
    for case in cases: case["acceptance"]=case["name"] in accept
    (output/"cases.json").write_text(json.dumps({"generator_version":1,"seed":42,"cases":cases},indent=2)+"\n")


if __name__ == "__main__":
    p=argparse.ArgumentParser();p.add_argument("--output",type=Path,required=True);p.add_argument("--helper",type=Path,required=True);p.add_argument("--binary",type=Path,required=True);p.add_argument("--events",default="100000,1000000");p.add_argument("--aurora",type=Path);p.add_argument("--accept",default="",help="comma-separated case names this run claims to optimize");a=p.parse_args()
    prepare(a.output,a.helper,a.binary,list(map(int,a.events.split(","))),a.aurora,[name for name in a.accept.split(",") if name])
