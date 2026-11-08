enum core.result.Result[T: linear, X: linear] derive [Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash] pub
    Ok(T)
    Err(X)
end core.result.Result

fn core.result.Result.and_then[F: core.ops.FnOnce<(T,), Result<U, X>, E, P>, T: linear, U: linear, X: linear, E: uses, P: panics](self: Result<T, X>, f: F) -> Result<U, X> uses [E] panics P pub
fn core.result.Result.as_ref[T: linear, X: linear](self: &Result<T, X>) -> Result<&T, &X> uses [] panics never pub
fn core.result.Result.is_err[T: linear, X: linear](self: &Result<T, X>) -> bool uses [] panics never pub
fn core.result.Result.is_ok[T: linear, X: linear](self: &Result<T, X>) -> bool uses [] panics never pub
fn core.result.Result.map[F: core.ops.FnOnce<(T,), U, E, P>, T: linear, U: linear, X: linear, E: uses, P: panics](self: Result<T, X>, f: F) -> Result<U, X> uses [E] panics P pub
fn core.result.Result.map_err[F: core.ops.FnOnce<(X,), Y, E, P>, T: linear, X: linear, Y: linear, E: uses, P: panics](self: Result<T, X>, f: F) -> Result<T, Y> uses [E] panics P pub
fn core.result.Result.ok[T: linear, X](self: Result<T, X>) -> Option<T> uses [] panics never pub
fn core.result.Result.unwrap[T: linear, X: linear + core.fmt.Debug](self: Result<T, X>) -> T uses [] panics may pub
fn core.result.Result.unwrap_or[T, X](self: Result<T, X>, default: T) -> T uses [] panics never pub
