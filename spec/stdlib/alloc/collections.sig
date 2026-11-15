type alloc.collections.BtreeMap[K: core.cmp.Ord, V, A: alloc.alloc.Allocator] pub
    entries: alloc.raw.Buf<alloc.collections.MapEntry<K, V>, A>
end alloc.collections.BtreeMap

type alloc.collections.MapEntry[K, V] pub
    key: K
    value: V
end alloc.collections.MapEntry

type alloc.collections.HashMap[K: core.cmp.Eq + core.hash.Hash, V, A: alloc.alloc.Allocator] pub
    cells: alloc.raw.Buf<alloc.collections.HashCell<K, V>, A>
    slots: usize
end alloc.collections.HashMap

enum alloc.collections.HashCell[K, V] pub
    Empty
    Index(usize)
    Entry(usize, K, V)
end alloc.collections.HashCell

type alloc.collections.HashMapIter['a, K, V] pub
    cells: &'a [alloc.collections.HashCell<K, V>]
    position: usize
end alloc.collections.HashMapIter

type alloc.collections.VecDeque[T, A: alloc.alloc.Allocator] pub
    allocator: A
    buffer: *mut T
    cap: usize
    head: usize
    used: usize
end alloc.collections.VecDeque

impl[K: core.cmp.Eq + core.hash.Hash + core.marker.Send, V: core.marker.Send, A: alloc.alloc.Allocator + core.marker.Send] core.marker.Send for alloc.collections.HashMap<K, V, A>
impl[K: core.cmp.Eq + core.hash.Hash + core.marker.Sync, V: core.marker.Sync, A: alloc.alloc.Allocator + core.marker.Sync] core.marker.Sync for alloc.collections.HashMap<K, V, A>
impl[K: core.cmp.Ord + core.marker.Send, V: core.marker.Send, A: alloc.alloc.Allocator + core.marker.Send] core.marker.Send for alloc.collections.BtreeMap<K, V, A>
impl[K: core.cmp.Ord + core.marker.Sync, V: core.marker.Sync, A: alloc.alloc.Allocator + core.marker.Sync] core.marker.Sync for alloc.collections.BtreeMap<K, V, A>
impl[T: core.marker.Send, A: alloc.alloc.Allocator + core.marker.Send] core.marker.Send for alloc.collections.VecDeque<T, A>
impl[T: core.marker.Sync, A: alloc.alloc.Allocator + core.marker.Sync] core.marker.Sync for alloc.collections.VecDeque<T, A>

fn alloc.collections.BtreeMap.new[K: core.cmp.Ord, V, A: alloc.alloc.Allocator](allocator: A) -> alloc.collections.BtreeMap<K, V, A> uses [] panics never pub
fn alloc.collections.HashMap.new[K: core.cmp.Eq + core.hash.Hash, V, A: alloc.alloc.Allocator](allocator: A) -> alloc.collections.HashMap<K, V, A> uses [] panics never pub
fn alloc.collections.VecDeque.is_empty[T, A: alloc.alloc.Allocator](self: &alloc.collections.VecDeque<T, A>) -> bool uses [] panics never pub
fn alloc.collections.VecDeque.len[T, A: alloc.alloc.Allocator](self: &alloc.collections.VecDeque<T, A>) -> usize uses [] panics never pub
fn alloc.collections.VecDeque.new[T, A: alloc.alloc.Allocator](allocator: A) -> alloc.collections.VecDeque<T, A> uses [] panics never pub
fn alloc.collections.VecDeque.pop_back[T, A: alloc.alloc.Allocator](self: &mut alloc.collections.VecDeque<T, A>) -> Option<T> uses [] panics never pub
fn alloc.collections.VecDeque.pop_front[T, A: alloc.alloc.Allocator](self: &mut alloc.collections.VecDeque<T, A>) -> Option<T> uses [] panics never pub
fn alloc.collections.VecDeque.push_back[T, A: alloc.alloc.Allocator](self: &mut alloc.collections.VecDeque<T, A>, value: T) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never pub
fn alloc.collections.VecDeque.push_front[T, A: alloc.alloc.Allocator](self: &mut alloc.collections.VecDeque<T, A>, value: T) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never pub
