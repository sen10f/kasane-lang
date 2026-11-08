type core.iter.Enumerate[I, T, E: uses, P: panics] pub
    it: I
    index: usize
end core.iter.Enumerate

type core.iter.Filter[I, F, T, E: uses, P: panics] pub
    it: I
    predicate: F
end core.iter.Filter

type core.iter.Map[I, F, T, U, E: uses, P: panics] pub
    it: I
    f: F
end core.iter.Map

type core.iter.Range[T] derive [Debug, Clone, PartialEq, Eq] pub
    start: T
    stop: T
end core.iter.Range

trait core.iter.Step requires core.cmp.Ord pub
    fn successor(self: &Self) -> Option<Self> uses [] panics never
end core.iter.Step

type core.iter.Take[I, T, E: uses, P: panics] pub
    it: I
    remaining: usize
end core.iter.Take

impl[I: core.iter.Iterator<T, E, P>, T, E: uses, P: panics] core.iter.Iterator<(usize, T), E, P> for core.iter.Enumerate<I, T, E, P>
fn core.iter.Enumerate.next[I: core.iter.Iterator<T, E, P>, T, E: uses, P: panics](self: &mut core.iter.Enumerate<I, T, E, P>) -> Option<(usize, T)> uses [E] panics P impls core.iter.Iterator<(usize, T), E, P>
impl[I: core.iter.Iterator<T, E, P>, F: core.ops.Fn<(&T,), bool, E, P>, T, E: uses, P: panics] core.iter.Iterator<T, E, P> for core.iter.Filter<I, F, T, E, P>
fn core.iter.Filter.next[I: core.iter.Iterator<T, E, P>, F: core.ops.Fn<(&T,), bool, E, P>, T, E: uses, P: panics](self: &mut core.iter.Filter<I, F, T, E, P>) -> Option<T> uses [E] panics P impls core.iter.Iterator<T, E, P>
impl[I: core.iter.Iterator<T, E, P>, F: core.ops.Fn<(T,), U, E, P>, T, U, E: uses, P: panics] core.iter.Iterator<U, E, P> for core.iter.Map<I, F, T, U, E, P>
fn core.iter.Map.next[I: core.iter.Iterator<T, E, P>, F: core.ops.Fn<(T,), U, E, P>, T, U, E: uses, P: panics](self: &mut core.iter.Map<I, F, T, U, E, P>) -> Option<U> uses [E] panics P impls core.iter.Iterator<U, E, P>
impl[T: core.iter.Step] core.iter.Iterator<T, [], never> for core.iter.Range<T>
fn core.iter.Range.next[T: core.iter.Step](self: &mut core.iter.Range<T>) -> Option<T> uses [] panics never impls core.iter.Iterator<T, [], never>
impl[I: core.iter.Iterator<T, E, P>, T, E: uses, P: panics] core.iter.Iterator<T, E, P> for core.iter.Take<I, T, E, P>
fn core.iter.Take.next[I: core.iter.Iterator<T, E, P>, T, E: uses, P: panics](self: &mut core.iter.Take<I, T, E, P>) -> Option<T> uses [E] panics P impls core.iter.Iterator<T, E, P>
fn core.iter.count[I: core.iter.Iterator<T, E, P>, T, E: uses, P: panics](it: I) -> usize uses [E] panics P pub
fn core.iter.enumerate[I: core.iter.Iterator<T, E, P>, T, E: uses, P: panics](it: I) -> core.iter.Enumerate<I, T, E, P> uses [] panics never pub
fn core.iter.filter[I: core.iter.Iterator<T, E, P>, F: core.ops.Fn<(&T,), bool, E, P>, T, E: uses, P: panics](it: I, predicate: F) -> core.iter.Filter<I, F, T, E, P> uses [] panics never pub
fn core.iter.fold[I: core.iter.Iterator<T, E, P>, F: core.ops.Fn<(B, T), B, E, P>, B, T, E: uses, P: panics](it: I, init: B, f: F) -> B uses [E] panics P pub
fn core.iter.map[I: core.iter.Iterator<T, E, P>, F: core.ops.Fn<(T,), U, E, P>, T, U, E: uses, P: panics](it: I, f: F) -> core.iter.Map<I, F, T, U, E, P> uses [] panics never pub
fn core.iter.range[T: core.iter.Step](start: T, stop: T) -> core.iter.Range<T> uses [] panics never pub
fn core.iter.take[I: core.iter.Iterator<T, E, P>, T, E: uses, P: panics](it: I, count: usize) -> core.iter.Take<I, T, E, P> uses [] panics never pub
