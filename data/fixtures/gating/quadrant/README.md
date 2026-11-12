# QuadrantGate fixtures

A synthetic sample and a Gating-ML 2.0 strategy with QuadrantGates, and
FlowKit 1.3.0 reference results, used by `cytiq-gating`'s
`tests/flowkit_quadrant_parity.rs`.

- `sample.fcs`: 3,000 events of the `../sample.fcs` design (seed + 1), with
  events exactly on the FSC-A/SSC-A dividers and on the FL2-A dividers -500
  and 0.
- `gates.xml`: hand-written and valid against FlowKit's bundled
  `Gating-ML.v2.0.xsd`. A 2-D QuadrantGate under a parent; a 1-D divider with
  two values listed out of order (three bins, `FCS` compensation and
  logicle), one quadrant located exactly on a divider value; dividers at a
  negative value and at zero; a gate whose parent is a quadrant; Boolean
  `and`/`not` over quadrants.
- `flowkit_reference.json`: per quadrant and gate, its FlowKit `gate_path`
  (which includes the QuadrantGate id), count and membership as packed bits
  in hex (`numpy.packbits`, most significant bit first). QuadrantGate
  containers have no membership and no entry.
- `cytiq_written.xml`: `to_gatingml` of the parsed strategy, pinned byte for
  byte by the Rust test; `parity/compare.py` checks that FlowKit reads it to
  the same membership.

FlowKit is the reference reader only. Its Gating-ML writer guesses quadrant
locations (`max/2`, `min*2`), which misplaces quadrants next to dividers at
zero or below. The raw-channel quadrants were also checked against plain
NumPy half-open intervals (`min <= x < max`).

Regenerate with (a new reference needs scientific review; never regenerate to
make an implementation pass):

```bash
parity/.env/py/bin/python parity/gen/gating_quadrant.py data/fixtures/gating/quadrant
```
