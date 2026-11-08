#!/usr/bin/env python3
"""Generate a tiny ONNX interface fixture, not a trained/validated OOD detector.

Development-only requirement: onnx. No Python is needed to run the Rust tests.
"""
from pathlib import Path

import onnx
from onnx import TensorProto, helper

graph = helper.make_graph(
    [helper.make_node("ReduceMean", ["features"], ["ood_score"], axes=[1], keepdims=1)],
    "software-contract-only",
    [helper.make_tensor_value_info("features", TensorProto.FLOAT, ["N", "F"])],
    [helper.make_tensor_value_info("ood_score", TensorProto.FLOAT, ["N", 1])],
)
model = helper.make_model(graph, producer_name="cytiq-engine-test-fixture",
                          opset_imports=[helper.make_opsetid("", 17)], ir_version=9)
onnx.checker.check_model(model)
out = Path("data/fixtures/classify_safety/mean-ood.onnx")
out.parent.mkdir(parents=True, exist_ok=True)
onnx.save(model, out)
graph = helper.make_graph(
    [helper.make_node("ReduceMean", ["features"], ["mean"], axes=[1], keepdims=1),
     helper.make_node("Div", ["mean", "zero"], ["ood_score"])],
    "invalid-score-software-fixture", graph.input, graph.output,
    [helper.make_tensor("zero", TensorProto.FLOAT, [1], [0.0])],
)
model = helper.make_model(graph, producer_name="cytiq-engine-test-fixture",
                          opset_imports=[helper.make_opsetid("", 17)], ir_version=9)
onnx.checker.check_model(model)
onnx.save(model, out.with_name("nonfinite-ood.onnx"))
