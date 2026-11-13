#!/usr/bin/env python3
"""Novelty sensitivity to event subsampling, keeping reference membership fixed.

Uniform seeded permutations change which source events the production sampler
sees at its default seed 42 (`cytoscan --seed`; `run.py novelty` varies that
seed separately). Counts above 1,000 exercise the actual production sampling
cap; smaller counts use every event.
"""
import argparse
import json
from pathlib import Path
import tempfile

import flowio
import numpy as np
import pandas as pd

from af_sensitivity import subset
from datasets import sha256
from metrics import wilson_interval
from run import CHANNELS, invoke


def run(binary, source, output):
    output.mkdir(parents=True,exist_ok=False)
    files={kind:sorted((source/kind).glob('*.fcs')) for kind in ('reference','test')}
    if len(files['reference']) < 32 or len(files['test']) < 8:
        raise ValueError('required independent synthetic normal acquisitions missing')
    report={'schema_version':1,'status':'failed','binary_sha256':sha256(binary),
            'source_sha256':{str(p):sha256(p) for group in files.values() for p in group},
            'seed_scope':'source event permutations; default algorithm seed 42; reference membership fixed',
            'results':[]}
    try:
        for budget in (250,1000,2000):
            for seed in (1,7,42,101,2026):
                with tempfile.TemporaryDirectory(dir=output) as directory:
                    root=Path(directory)
                    paths={kind:[] for kind in files}
                    for kind,group in files.items():
                        (root/kind).mkdir()
                        for i,path in enumerate(group[:32] if kind=='reference' else group):
                            count=flowio.FlowData(str(path),only_text=True).event_count
                            target=root/kind/path.name
                            rows=np.random.default_rng(seed+100003*i).permutation(count)[:min(count,budget)]
                            subset(path,target,rows)
                            paths[kind].append(target)
                    for n in (8,32):
                        for method in ('summary','quantiles'):
                            case=output/f'{budget}-{seed}-{n}-{method}'
                            args=['cytoscan','--output',case,'--channels',','.join(CHANNELS),'--method',method,'--flag-method','novelty']
                            for path in paths['reference'][:n]:args+=['--reference',path]
                            for path in paths['test']:args+=['--input',path]
                            invoke(binary,args,output/f'{case.name}.log')
                            summary=pd.read_csv(case/'cytoscan_summary.csv')
                            hits=int(summary['is_novelty'].astype(str).str.lower().isin(['true','1']).sum())
                            report['results'].append({'events_available':budget,'production_events':min(1000,budget),'event_seed':seed,'reference_files':n,'method':method,'normal_files':len(summary),'false_positives':hits,'fpr':hits/len(summary),'wilson_ci95':wilson_interval(hits,len(summary))})
                (output/'progress.json').write_text(json.dumps(report,indent=2,allow_nan=False)+'\n')
        report['status']='completed'
    except Exception as error:
        report['error']=str(error)
        raise
    finally:
        (output/'report.json').write_text(json.dumps(report,indent=2,allow_nan=False)+'\n')


if __name__ == '__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--source',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
    run(a.binary,a.source,a.output)
