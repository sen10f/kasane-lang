type core.slice.IndexError derive [Debug, Clone, Copy, PartialEq, Eq] pub
    index: usize pub
end core.slice.IndexError

type core.slice.Iter['a, T] pub
    items: &'a [T]
end core.slice.Iter

type core.slice.LengthError derive [Debug, Clone, Copy, PartialEq, Eq] pub
    expected: usize pub
end core.slice.LengthError

fn core.slice.Iter.count[T](self: core.slice.Iter<T>) -> usize uses [] panics never pub
impl['a, T] core.iter.Iterator<&'a T, [], never> for core.slice.Iter<'a, T>
fn core.slice.Iter.next['a, T](self: &mut core.slice.Iter<'a, T>) -> Option<&'a T> uses [] panics never impls core.iter.Iterator<&'a T, [], never>
fn core.slice.binary_search[T: core.cmp.Ord](self: &[T], x: &T) -> Result<usize, usize> uses [] panics never pub
fn core.slice.contains[T: core.cmp.PartialEq<T>](self: &[T], value: &T) -> bool uses [] panics never pub
fn core.slice.copy_from[T: core.ops.Copy](self: &mut [T], source: &[T]) -> Result<(), core.slice.LengthError> uses [] panics never pub
fn core.slice.first[T](self: &[T]) -> Option<&T> uses [] panics never pub
fn core.slice.get[T](self: &[T], index: usize) -> Option<&T> uses [] panics never pub
fn core.slice.get_mut[T](self: &mut [T], index: usize) -> Option<&mut T> uses [] panics never pub
fn core.slice.is_empty[T](self: &[T]) -> bool uses [] panics never pub
fn core.slice.iter['a, T](self: &'a [T]) -> core.slice.Iter<'a, T> uses [] panics never pub
fn core.slice.last[T](self: &[T]) -> Option<&T> uses [] panics never pub
fn core.slice.len[T](self: &[T]) -> usize uses [] panics never pub
fn core.slice.reverse[T](self: &mut [T]) -> () uses [] panics never pub
fn core.slice.sort[T: core.cmp.Ord](self: &mut [T]) -> () uses [] panics never pub
fn core.slice.sort_by[T, F: core.ops.Fn<(&T, &T), core.cmp.Ordering, E, P>, E: uses, P: panics](self: &mut [T], compare: F) -> () uses [E] panics P pub
fn core.slice.sort_unstable[T: core.cmp.Ord](self: &mut [T]) -> () uses [] panics never pub
fn core.slice.sort_unstable_by[T, F: core.ops.Fn<(&T, &T), core.cmp.Ordering, E, P>, E: uses, P: panics](self: &mut [T], compare: F) -> () uses [E] panics P pub
fn core.slice.split_at[T](self: &[T], mid: usize) -> Option<(&[T], &[T])> uses [] panics never pub
fn core.slice.sub[T](self: &[T], from: usize, to: usize) -> Option<&[T]> uses [] panics never pub
fn core.slice.sub_mut[T](self: &mut [T], from: usize, to: usize) -> Option<&mut [T]> uses [] panics never pub
fn core.slice.swap[T](self: &mut [T], first: usize, second: usize) -> Result<(), core.slice.IndexError> uses [] panics never pub
