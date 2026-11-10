#!/usr/bin/env python3
"""Real Aurora AF subsampling ablation; preserves detector ranges and gains.

Controls other than the unstained acquisition stay fixed. The same 4,000 sample
events are unmixed in every run. No reference or production solver is changed.
"""
import argparse
import json
import os
from pathlib import Path
import tempfile

import flowio
import numpy as np
import pandas as pd

from datasets import sha256
from run import invoke, references


def subset(source, output, rows):
    flow = flowio.FlowData(str(source))
    if flow.text['datatype'] != 'F':
        raise ValueError('ablation requires lossless float32 input')
    names = [flow.text[f'p{i+1}n'] for i in range(flow.channel_count)]
    labels = [flow.text.get(f'p{i+1}s', '') for i in range(flow.channel_count)]
    for i in range(flow.channel_count):
        if any(float(x) != 0 for x in flow.text[f'p{i+1}e'].split(',')):
            raise ValueError('log-amplified control requires a different writer')
    data = np.asarray(flow.events).reshape(-1,flow.channel_count)[rows]
    with output.open('wb') as handle:
        flowio.create_fcs(handle, data.ravel().tolist(), names, labels, flow.text)
    reread = flowio.FlowData(str(output))
    if not np.array_equal(np.asarray(reread.events).reshape(data.shape),data):
        raise ValueError('subset changed raw values')
    for i in range(flow.channel_count):
        for suffix in ('n','r','e','g','s'):
            key = f'p{i+1}{suffix}'
            if key in flow.text and reread.text.get(key) != flow.text[key]:
                raise ValueError(f'subset changed detector metadata: {key}')


def run(binary, source, output, seeds):
    output.mkdir(parents=True,exist_ok=False)
    controls = pd.read_csv(source/'control_file.csv',dtype=str).fillna('')
    af_name = controls.loc[controls.fluorophore == 'AF','filename'].item()
    af = source/'controls'/af_name
    sample = next((source/'sample').glob('*.fcs'))
    count = flowio.FlowData(str(af), only_text=True).event_count
    sample_count = flowio.FlowData(str(sample), only_text=True).event_count
    fixed_sample = output/'sample.fcs'
    subset(sample,fixed_sample,np.sort(np.random.default_rng(42).choice(sample_count,min(4000,sample_count),replace=False)))
    baseline = None
    report = {'schema_version':1,'status':'failed','binary_sha256':sha256(binary),
              'controls':{p.name:sha256(p) for p in (source/'controls').glob('*.fcs')},
              'sample_sha256':sha256(sample),'sample_events':min(4000,sample_count),'results':[]}
    try:
        for size, seed in [(count,42)]+[(n,s) for n in (1000,10000) if n < count for s in range(1,seeds+1)]:
            with tempfile.TemporaryDirectory(dir=output) as directory:
                root = Path(directory)
                sampled_af = root/'af.fcs'
                subset(af,sampled_af,np.sort(np.random.default_rng(seed).choice(count,size,replace=False)))
                manifest = controls.copy()
                manifest['filename'] = [os.path.relpath(sampled_af if name == af_name else source/'controls'/name,root) for name in controls.filename]
                manifest['universal.negative'] = [os.path.relpath(sampled_af if name == af_name else source/'controls'/name,root) if name else '' for name in controls['universal.negative']]
                manifest.to_csv(root/'controls.csv',index=False)
                pd.DataFrame({'filename':[os.path.relpath(fixed_sample,root)]}).to_csv(root/'samples.csv',index=False)
                result = root/'result'
                invoke(binary,['autospectral','--controls',root/'controls.csv','--samples',root/'samples.csv','--input-dir',root,'--output',result],output/f'{size}-{seed}.log')
                reference = references(result)
                summary = json.loads((result/'diagnostics/run.json').read_text())
                unmixed = next((result/'unmixed').glob('*.fcs'))
                flow = flowio.FlowData(str(unmixed))
                values = np.asarray(flow.events).reshape(-1,flow.channel_count)
                if baseline is None:
                    baseline = (reference,values)
                cosine = np.sum(reference*baseline[0],axis=0)/(np.linalg.norm(reference,axis=0)*np.linalg.norm(baseline[0],axis=0))
                delta = values-baseline[1]
                report['results'].append({'af_events':size,'seed':seed,
                    'diagnostics':summary['reference_diagnostics'],
                    'component_cosine_to_full':cosine.tolist(),
                    'unmixed_rms_delta':np.sqrt(np.mean(delta**2,axis=0)).tolist(),
                    'unmixed_relative_rms_delta':(np.sqrt(np.mean(delta**2,axis=0))/np.maximum(np.sqrt(np.mean(baseline[1]**2,axis=0)),1e-12)).tolist()})
                (output/'progress.json').write_text(json.dumps(report,indent=2,allow_nan=False)+'\n')
        report['status'] = 'completed'
    except Exception as error:
        report['error'] = str(error)
        raise
    finally:
        (output/'report.json').write_text(json.dumps(report,indent=2,allow_nan=False)+'\n')


if __name__ == '__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--source',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--seeds',type=int,default=20);a=p.parse_args()
    run(a.binary,a.source,a.output,a.seeds)
