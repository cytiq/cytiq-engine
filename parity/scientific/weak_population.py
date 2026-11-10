#!/usr/bin/env python3
"""Filtering sensitivity to an in-silico weak CD19 phenotype.

Reuse the labelled pipeline acquisition and technical truth. Attenuate measured
compensated CD19 by 20x in B/rare cells and remix the declared spillover. This is
a phenotype perturbation, not independent experimental weak-positive truth.
"""
import argparse
import json
from pathlib import Path

import flowio
import numpy as np
import pandas as pd
import pyarrow.parquet as pq

from datasets import sha256
from metrics import retention
from run import ROOT, invoke, read_fcs


def evaluate(binary,source,root,labels,good):
    root.mkdir()
    channels=','.join(f'FL{i}-A' for i in range(1,6))
    commands=[['compensate','--input',source,'--output',root/'comp.fcs'],
              ['transform','--input',root/'comp.fcs','--output',root/'transformed.fcs'],
              ['peacoqc','--input',root/'transformed.fcs','--output',root/'peacoqc.fcs','--channels',channels],
              ['clean','--input',source,'--output',root/'clean']]
    for i,args in enumerate(commands):invoke(binary,args,root/f'{i}.log')
    stages=[];acquisition=None
    for name,path in [('margins',root/'comp.masks.parquet'),('peacoqc',root/'peacoqc.masks.parquet'),('clean',root/'clean/masks.parquet')]:
        table=pq.read_table(path)
        current=pq.read_metadata(path).metadata[b'cytiq.acquisition_sha256']
        if acquisition is None:acquisition=current
        if acquisition != current:raise ValueError('mixed filtering acquisitions')
        data=table.to_pandas();ids=data.source_event_id.to_numpy(dtype=np.int64)
        if len(np.unique(ids))!=len(labels) or set(ids)!=set(range(len(labels))):raise ValueError('incomplete source lineage')
        keep=np.zeros(len(labels),dtype=bool);keep[ids]=data.keep.to_numpy(dtype=bool)
        stages.append((name,keep))
    report=retention(labels,stages,good)
    combined=np.logical_and.reduce([mask for _,mask in stages])
    return {'stages':report,'technical_good_retention':float(np.mean(combined[good])),
            'technical_artifact_removal':float(np.mean(~combined[~good]))}


def run(binary,output,baseline=None):
    output.mkdir(parents=True,exist_ok=False)
    fixtures=ROOT/'data/fixtures/pipeline'
    source=fixtures/'sample.fcs';truth=pd.read_csv(fixtures/'truth/events.csv')
    values,names=read_fcs(source);flow=flowio.FlowData(str(source))
    if any(k.lower().startswith('cytiq_') for k in flow.text):raise ValueError('expected raw synthetic acquisition')
    dyes=[f'FL{i}-A' for i in range(1,7)];indices=[names.index(c) for c in dyes]
    matrix=pd.read_csv(fixtures/'truth/spillover.csv',index_col=0)
    if list(matrix.columns)!=dyes:raise ValueError('spillover detector order changed')
    phenotypes=truth.population.isin(['b','rare']).to_numpy()
    measured=np.linalg.solve(matrix.to_numpy().T,values[:,indices].T).T
    measured[phenotypes,3]*=.05
    values[:,indices]=measured@matrix.to_numpy()
    weak=output/'weak.fcs'
    with weak.open('wb') as handle:
        flowio.create_fcs(handle,values.ravel().tolist(),names,[flow.text.get(f'p{i+1}s','') for i in range(len(names))],flow.text)
    labels=truth.population.astype(object).to_numpy(copy=True);labels[labels=='none']=None
    good=~truth[['debris','doublet','dead','unstable','margin']].astype(bool).any(axis=1).to_numpy()
    report={'schema_version':1,'status':'failed','binary_sha256':sha256(binary),
            'source_sha256':sha256(source),'truth_sha256':sha256(fixtures/'truth/events.csv'),
            'weak_source_sha256':sha256(weak),'attenuation':.05,'positive_marker':'CD19',
            'weak_populations':['b','rare'],'independent_experimental_weak_truth':'not_assessed',
            'biological_safety':'not_established_by_this_perturbation','cases':{}}
    try:
        reference=json.loads((ROOT/'data/fixtures/scientific/weak-retention.json').read_text())
        for key in ('source_sha256','truth_sha256','attenuation','positive_marker'):
            if reference[key]!=report[key]:raise ValueError(f'weak-phenotype reference inputs changed: {key}')
        for name,path in [('original',source),('weak',weak)]:
            report['cases'][name]=evaluate(binary,path,output/name,labels,good)
            if report['cases'][name]!=reference['cases'][name]:raise ValueError(f'{name}: filtering differs from frozen clean-main metrics; scientific review required')
            if baseline is not None:
                oracle=evaluate(baseline,path,output/(name+'-baseline'),labels,good)
                if oracle!=report['cases'][name]:raise ValueError(f'{name}: population filtering changed from clean-main baseline')
        if baseline is not None:report['baseline']={'sha256':sha256(baseline),'population_metrics_identical':True}
        report['status']='completed'
    except Exception as error:
        report['error']=str(error)
        raise
    finally:
        (output/'report.json').write_text(json.dumps(report,indent=2,allow_nan=False)+'\n')


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--baseline-bin',type=Path);p.add_argument('--output',type=Path,required=True);a=p.parse_args();run(a.binary,a.output,a.baseline_bin)
