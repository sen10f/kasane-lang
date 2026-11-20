fn core.ffi.trampoline[F: core.ops.FnMut<(A,), R, E, P>, A, R, E: uses, P: panics](closure: &mut F) -> (fn(*mut (), A) -> R uses [] panics never, *mut ()) uses [E] panics P pub
