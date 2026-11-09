#!/usr/bin/env python3
"""Explicit units/rescaling check of the existing raw AutoSpread surrogate."""
import argparse
import json
from pathlib import Path

import numpy as np
import pandas as pd

from datasets import sha256
from run import invoke, read_fcs, write_fcs


def run(binary,source,output):
    output.mkdir(parents=True,exist_ok=False)
    controls=pd.read_csv(source/'controls.csv')
    estimates=[]
    report={'schema_version':1,'status':'failed','binary_sha256':sha256(binary),
            'inputs':{name:sha256(source/name) for name in controls.filename},
            'orientation':'rows=dyes, columns=detectors',
            'comparison':'both primary and secondary ADC values scaled by k; homogeneous SD/sqrt(signal) would scale by sqrt(k)',
            'actual_estimator':'two regressions using sqrt_like(x)=sign(x)*(sqrt(abs(x)+1)-1); asymptotic units only',
            'results':[]}
    try:
        for scale in (.01,1.,100.):
            root=output/str(scale);root.mkdir()
            controls.to_csv(root/'controls.csv',index=False)
            for name in controls.filename:
                values,names=read_fcs(source/name)
                # Synthetic controls declare two scatter columns followed by
                # fluorescence. Preserve scatter and scale detector ranges.
                if names[:2] != ['FSC-A','SSC-A']:raise ValueError('unexpected units fixture')
                values[:,2:]*=scale
                write_fcs(root/name,values,names,extra_keywords={f'p{i+1}r':str(262144*scale) for i in range(2,len(names))})
            path=root/'spread.csv'
            invoke(binary,['autospread','--controls',root/'controls.csv','--input-dir',root,'--output',path],root/'run.log')
            estimates.append(pd.read_csv(path,index_col=0).to_numpy())
        baseline=estimates[1]
        for scale,matrix in zip((.01,1.,100.),estimates):
            if matrix.shape != baseline.shape or not np.isfinite(matrix).all():raise ValueError('invalid spread matrix')
            report['results'].append({'scale':scale,'estimated':matrix.tolist(),'max_absolute_nonhomogeneity':float(np.abs(matrix/np.sqrt(scale)-baseline).max())})
        report['status']='completed'
    except Exception as error:
        report['error']=str(error)
        raise
    finally:
        (output/'report.json').write_text(json.dumps(report,indent=2,allow_nan=False)+'\n')


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--source',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args();run(a.binary,a.source,a.output)
