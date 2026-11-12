"""Gating-ML 2.0 QuadrantGate fixtures with FlowKit reference results.

Usage: python gating_quadrant.py <out_dir>

Writes `sample.fcs`, `gates.xml` and `flowkit_reference.json` into `out_dir`
(committed as `data/fixtures/gating/quadrant/`). The XML is written here by
hand and validated against the Gating-ML 2.0 schema bundled with FlowKit:
FlowKit's own Gating-ML writer guesses quadrant locations (`max/2`,
`min*2`), which places them in the wrong bin for dividers at zero or below,
so it is not used to produce fixtures. FlowKit is the reference *reader*:
per-quadrant membership comes from `GatingStrategy.gate_sample`.

Covered: a 2-D QuadrantGate under a parent gate; a 1-D divider with two
values listed out of order (three bins, compensated and logicle-scaled), one
quadrant located exactly on a divider value; dividers at a negative value and
at zero; events exactly on divider values; a gate whose parent is a quadrant;
Boolean `and`/`not` over quadrants.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np

from fcsio import write_fcs
from gating import CHANNELS, LABELS, SEED, SPILL, TIMESTEP, _events

GATES_XML = """<?xml version="1.0" encoding="UTF-8"?>
<gating:Gating-ML
    xmlns:gating="http://www.isac-net.org/std/Gating-ML/v2.0/gating"
    xmlns:transforms="http://www.isac-net.org/std/Gating-ML/v2.0/transformations"
    xmlns:data-type="http://www.isac-net.org/std/Gating-ML/v2.0/datatypes">

  <transforms:transformation transforms:id="logicle">
    <transforms:logicle transforms:T="262144" transforms:W="0.5" transforms:M="4.5" transforms:A="0"/>
  </transforms:transformation>

  <gating:RectangleGate gating:id="Cells">
    <gating:dimension gating:min="20000" gating:compensation-ref="uncompensated">
      <data-type:fcs-dimension data-type:name="FSC-A"/>
    </gating:dimension>
  </gating:RectangleGate>

  <!-- 2-D quadrants; events sit exactly on both divider values. -->
  <gating:QuadrantGate gating:id="Quad" gating:parent_id="Cells">
    <gating:divider gating:id="FSC_divider" gating:compensation-ref="uncompensated">
      <data-type:fcs-dimension data-type:name="FSC-A"/>
      <gating:value>100000</gating:value>
    </gating:divider>
    <gating:divider gating:id="SSC_divider" gating:compensation-ref="uncompensated">
      <data-type:fcs-dimension data-type:name="SSC-A"/>
      <gating:value>50000</gating:value>
    </gating:divider>
    <gating:Quadrant gating:id="FSChi_SSChi">
      <gating:position gating:divider_ref="FSC_divider" gating:location="150000"/>
      <gating:position gating:divider_ref="SSC_divider" gating:location="80000"/>
    </gating:Quadrant>
    <gating:Quadrant gating:id="FSChi_SSClo">
      <gating:position gating:divider_ref="FSC_divider" gating:location="150000"/>
      <gating:position gating:divider_ref="SSC_divider" gating:location="20000"/>
    </gating:Quadrant>
    <gating:Quadrant gating:id="FSClo_SSChi">
      <gating:position gating:divider_ref="FSC_divider" gating:location="50000"/>
      <gating:position gating:divider_ref="SSC_divider" gating:location="80000"/>
    </gating:Quadrant>
    <gating:Quadrant gating:id="FSClo_SSClo">
      <gating:position gating:divider_ref="FSC_divider" gating:location="50000"/>
      <gating:position gating:divider_ref="SSC_divider" gating:location="20000"/>
    </gating:Quadrant>
  </gating:QuadrantGate>

  <!-- One divider, two values out of order: three bins on compensated,
       logicle-scaled FL1-A. `CD3_mid` is located exactly on a value. -->
  <gating:QuadrantGate gating:id="CD3_bins">
    <gating:divider gating:id="CD3_divider" gating:compensation-ref="FCS" gating:transformation-ref="logicle">
      <data-type:fcs-dimension data-type:name="FL1-A"/>
      <gating:value>0.6</gating:value>
      <gating:value>0.3</gating:value>
    </gating:divider>
    <gating:Quadrant gating:id="CD3_low">
      <gating:position gating:divider_ref="CD3_divider" gating:location="0.1"/>
    </gating:Quadrant>
    <gating:Quadrant gating:id="CD3_mid">
      <gating:position gating:divider_ref="CD3_divider" gating:location="0.3"/>
    </gating:Quadrant>
    <gating:Quadrant gating:id="CD3_high">
      <gating:position gating:divider_ref="CD3_divider" gating:location="0.9"/>
    </gating:Quadrant>
  </gating:QuadrantGate>

  <!-- Dividers at a negative value and at zero, raw FL2-A. -->
  <gating:QuadrantGate gating:id="FL2_sign" gating:parent_id="Cells">
    <gating:divider gating:id="FL2_divider" gating:compensation-ref="uncompensated">
      <data-type:fcs-dimension data-type:name="FL2-A"/>
      <gating:value>-500</gating:value>
      <gating:value>0</gating:value>
    </gating:divider>
    <gating:Quadrant gating:id="FL2_below">
      <gating:position gating:divider_ref="FL2_divider" gating:location="-1000"/>
    </gating:Quadrant>
    <gating:Quadrant gating:id="FL2_between">
      <gating:position gating:divider_ref="FL2_divider" gating:location="-250"/>
    </gating:Quadrant>
    <gating:Quadrant gating:id="FL2_above">
      <gating:position gating:divider_ref="FL2_divider" gating:location="0"/>
    </gating:Quadrant>
  </gating:QuadrantGate>

  <gating:RectangleGate gating:id="HiHi_FL3" gating:parent_id="FSChi_SSChi">
    <gating:dimension gating:min="1000" gating:compensation-ref="uncompensated">
      <data-type:fcs-dimension data-type:name="FL3-A"/>
    </gating:dimension>
  </gating:RectangleGate>

  <gating:BooleanGate gating:id="HiHi_and_CD3mid">
    <gating:and>
      <gating:gateReference gating:ref="FSChi_SSChi"/>
      <gating:gateReference gating:ref="CD3_mid"/>
    </gating:and>
  </gating:BooleanGate>

  <gating:BooleanGate gating:id="not_LoLo" gating:parent_id="Cells">
    <gating:not>
      <gating:gateReference gating:ref="FSClo_SSClo"/>
    </gating:not>
  </gating:BooleanGate>
</gating:Gating-ML>
"""

EVENTS = 3000


def events() -> np.ndarray:
    rng = np.random.default_rng(SEED + 1)
    data = _events(rng)[:EVENTS].copy()
    # Exact divider hits (raw values, float32-exact): FSC/SSC corners and the
    # FL2-A dividers -500 and 0.
    data[0, :2] = [100000, 50000]
    data[1, :2] = [100000, 20000]
    data[2, :2] = [50000, 50000]
    data[3:6, 0] = 120000
    data[3:6, 3] = [-500, 0, -499.5]
    return data.astype(np.float32).astype(np.float64)


def validate(xml: Path) -> None:
    """Validate against the Gating-ML 2.0 XSD shipped with FlowKit."""
    import flowkit
    from lxml import etree

    resources = Path(flowkit.__file__).parent / "_resources"
    schema = etree.XMLSchema(etree.parse(str(resources / "Gating-ML.v2.0.xsd")))
    schema.assertValid(etree.parse(str(xml)))


def membership(results, gate_id: str, gate_path: tuple) -> np.ndarray | None:
    """FlowKit's membership of one gate; `None` for a QuadrantGate container.

    FlowKit 1.3's `GatingResults.get_gate_membership` misroutes ordinary gates
    once a strategy holds a QuadrantGate: pandas stores their missing
    `quadrant_parent` as NaN, and `None in {nan}` is false. Read the raw
    results the same way that method does, keyed on a proper NaN check.
    """
    import pandas as pd

    report = results.report
    rows = report[report.gate_name == gate_id]
    if rows.empty:
        return None  # a QuadrantGate itself has no membership
    paths = results._gate_lut[gate_id]["paths"]
    path = paths[0] if len(paths) == 1 else "/".join(gate_path)
    parent = rows.quadrant_parent.iloc[0]
    if pd.isna(parent):
        return np.asarray(results._raw_results[gate_id, path]["events"], dtype=bool)
    return np.asarray(results._raw_results[parent, path][gate_id]["events"], dtype=bool)


def flowkit_reference(fcs: Path, xml: Path) -> dict:
    import flowkit as fk

    sample = fk.Sample(str(fcs))
    strategy = fk.parse_gating_xml(str(xml))
    results = strategy.gate_sample(sample)
    gates = {}
    for gate_id, gate_path in strategy.get_gate_ids():
        events = membership(results, gate_id, gate_path)
        if events is None:
            continue
        gates[gate_id] = {
            "gate_path": "/".join(list(gate_path[1:]) + [gate_id]),
            "count": int(events.sum()),
            "membership_hex": np.packbits(events).tobytes().hex(),
        }
    return {"event_count": int(sample.event_count), "gates": gates}


def main(out: Path) -> None:
    out.mkdir(parents=True, exist_ok=True)
    spill = ",".join(f"{value:g}" for value in SPILL.ravel())
    fcs = out / "sample.fcs"
    write_fcs(
        fcs,
        events(),
        CHANNELS,
        labels=LABELS,
        extra_keywords={"spillover": f"3,FL1-A,FL2-A,FL3-A,{spill}", "timestep": str(TIMESTEP)},
    )
    xml = out / "gates.xml"
    xml.write_text(GATES_XML)
    validate(xml)
    reference = flowkit_reference(fcs, xml)
    (out / "flowkit_reference.json").write_text(json.dumps(reference, indent=2) + "\n")


if __name__ == "__main__":
    main(Path(sys.argv[1]))
