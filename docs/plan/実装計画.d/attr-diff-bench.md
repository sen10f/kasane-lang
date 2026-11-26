# tests/attr-diff-bench

- KSN_LLVM_NO_ATTRS=1 (debug か test-hooks のみ): kir_llvm.rs の strip_attrs が IR から約束を外す。
- tests/attr_diff.py (差分試験、--fast は run_tests.py に接続)、bench/quiet_run.py (静かな時の計測)。README 参照。
