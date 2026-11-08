enum core.option.Option[T: linear] derive [Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash] pub
    Some(T)
    None
end core.option.Option

fn core.option.Option.and_then[F: core.ops.FnOnce<(T,), Option<U>, E, P>, T: linear, U: linear, E: uses, P: panics](self: Option<T>, f: F) -> Option<U> uses [E] panics P pub
fn core.option.Option.as_mut[T: linear](self: &mut Option<T>) -> Option<&mut T> uses [] panics never pub
fn core.option.Option.as_ref[T: linear](self: &Option<T>) -> Option<&T> uses [] panics never pub
fn core.option.Option.is_none[T: linear](self: &Option<T>) -> bool uses [] panics never pub
fn core.option.Option.is_some[T: linear](self: &Option<T>) -> bool uses [] panics never pub
fn core.option.Option.map[F: core.ops.FnOnce<(T,), U, E, P>, T: linear, U: linear, E: uses, P: panics](self: Option<T>, f: F) -> Option<U> uses [E] panics P pub
fn core.option.Option.ok_or[T: linear, X](self: Option<T>, error: X) -> Result<T, X> uses [] panics never pub
fn core.option.Option.take[T: linear](self: &mut Option<T>) -> Option<T> uses [] panics never pub
fn core.option.Option.unwrap[T: linear](self: Option<T>) -> T uses [] panics may pub
fn core.option.Option.unwrap_or[T](self: Option<T>, default: T) -> T uses [] panics never pub
