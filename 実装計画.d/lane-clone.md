# vm/lane-clone

`&String.clone()` (kir_lower_coll の String clone) と `Option<&T>`/`Option<スカラー>` の clone (`clone_copy_opt`) を KIR に下ろす。試験: stage0/tests/vm_clone.rs (木走査 == VM == native)。copy_clone_shared_ok は次の理由 (総称関数へのクロージャ引数) が閉包の線の担当。
