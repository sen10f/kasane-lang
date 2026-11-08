fn core.ptr.add[T](pointer: *mut T, count: usize) -> *mut T uses [] panics never pub
fn core.ptr.as_const[T](pointer: *mut T) -> *const T uses [] panics never pub
fn core.ptr.as_mut[T](pointer: *const T) -> *mut T uses [] panics never pub
fn core.ptr.cast[T, U](pointer: *mut T) -> *mut U uses [] panics never pub
fn core.ptr.from_mut[T](reference: &mut T) -> *mut T uses [] panics never pub
fn core.ptr.from_ref[T](reference: &T) -> *const T uses [] panics never pub
fn core.ptr.is_null[T](pointer: *const T) -> bool uses [] panics never pub
fn core.ptr.null_mut[T]() -> *mut T uses [] panics never pub
fn core.ptr.read[T](pointer: *const T) -> T uses [] panics never pub
fn core.ptr.write[T](pointer: *mut T, value: T) -> () uses [] panics never pub
