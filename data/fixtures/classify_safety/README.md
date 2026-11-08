# Classifier safety interface fixtures

`mean-ood.onnx` reduces each feature row to a scalar mean. It is a software
interface fixture, **not a trained or scientifically validated OOD detector**.
`nonfinite-ood.onnx` divides that mean by zero to exercise failure and rollback.
Generate both with `python parity/gen/classify_ood.py` in a development environment
containing `onnx`. Rust tests need no Python or ONNX installation.

Tests construct synthetic validation reports for artifact-contract checks only.
They establish no clinical operating point or model performance claim.
