type alloc.collections.BtreeMap[K: core.cmp.Ord, V, A: alloc.alloc.Allocator] pub
    allocator: A
    root: *mut u8
    used: usize
end alloc.collections.BtreeMap

type alloc.collections.HashMap[K: core.cmp.Eq + core.hash.Hash, V, A: alloc.alloc.Allocator] pub
    allocator: A
    table: *mut u8
    cap: usize
    used: usize
end alloc.collections.HashMap

type alloc.collections.HashMapIter['a, K, V] pub
    table: *const u8
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

fn alloc.collections.BtreeMap.contains_key[K: core.cmp.Ord, V, A: alloc.alloc.Allocator](self: &alloc.collections.BtreeMap<K, V, A>, key: &K) -> bool uses [] panics never pub
fn alloc.collections.BtreeMap.get[K: core.cmp.Ord, V, A: alloc.alloc.Allocator](self: &alloc.collections.BtreeMap<K, V, A>, key: &K) -> Option<&V> uses [] panics never pub
fn alloc.collections.BtreeMap.get_mut[K: core.cmp.Ord, V, A: alloc.alloc.Allocator](self: &mut alloc.collections.BtreeMap<K, V, A>, key: &K) -> Option<&mut V> uses [] panics never pub
fn alloc.collections.BtreeMap.insert[K: core.cmp.Ord, V, A: alloc.alloc.Allocator](self: &mut alloc.collections.BtreeMap<K, V, A>, key: K, value: V) -> Result<Option<V>, alloc.alloc.AllocError> uses [alloc] panics never pub
fn alloc.collections.BtreeMap.is_empty[K: core.cmp.Ord, V, A: alloc.alloc.Allocator](self: &alloc.collections.BtreeMap<K, V, A>) -> bool uses [] panics never pub
fn alloc.collections.BtreeMap.len[K: core.cmp.Ord, V, A: alloc.alloc.Allocator](self: &alloc.collections.BtreeMap<K, V, A>) -> usize uses [] panics never pub
fn alloc.collections.BtreeMap.new[K: core.cmp.Ord, V, A: alloc.alloc.Allocator](allocator: A) -> alloc.collections.BtreeMap<K, V, A> uses [] panics never pub
fn alloc.collections.BtreeMap.remove[K: core.cmp.Ord, V, A: alloc.alloc.Allocator](self: &mut alloc.collections.BtreeMap<K, V, A>, key: &K) -> Option<V> uses [] panics never pub
fn alloc.collections.HashMap.contains_key[K: core.cmp.Eq + core.hash.Hash, V, A: alloc.alloc.Allocator](self: &alloc.collections.HashMap<K, V, A>, key: &K) -> bool uses [] panics never pub
fn alloc.collections.HashMap.get[K: core.cmp.Eq + core.hash.Hash, V, A: alloc.alloc.Allocator](self: &alloc.collections.HashMap<K, V, A>, key: &K) -> Option<&V> uses [] panics never pub
fn alloc.collections.HashMap.get_mut[K: core.cmp.Eq + core.hash.Hash, V, A: alloc.alloc.Allocator](self: &mut alloc.collections.HashMap<K, V, A>, key: &K) -> Option<&mut V> uses [] panics never pub
fn alloc.collections.HashMap.insert[K: core.cmp.Eq + core.hash.Hash, V, A: alloc.alloc.Allocator](self: &mut alloc.collections.HashMap<K, V, A>, key: K, value: V) -> Result<Option<V>, alloc.alloc.AllocError> uses [alloc] panics never pub
fn alloc.collections.HashMap.is_empty[K: core.cmp.Eq + core.hash.Hash, V, A: alloc.alloc.Allocator](self: &alloc.collections.HashMap<K, V, A>) -> bool uses [] panics never pub
fn alloc.collections.HashMap.iter['a, K: core.cmp.Eq + core.hash.Hash, V, A: alloc.alloc.Allocator](self: &'a alloc.collections.HashMap<K, V, A>) -> alloc.collections.HashMapIter<'a, K, V> uses [] panics never pub
fn alloc.collections.HashMap.len[K: core.cmp.Eq + core.hash.Hash, V, A: alloc.alloc.Allocator](self: &alloc.collections.HashMap<K, V, A>) -> usize uses [] panics never pub
fn alloc.collections.HashMap.new[K: core.cmp.Eq + core.hash.Hash, V, A: alloc.alloc.Allocator](allocator: A) -> alloc.collections.HashMap<K, V, A> uses [] panics never pub
fn alloc.collections.HashMap.remove[K: core.cmp.Eq + core.hash.Hash, V, A: alloc.alloc.Allocator](self: &mut alloc.collections.HashMap<K, V, A>, key: &K) -> Option<V> uses [] panics never pub
impl['a, K, V] core.iter.Iterator<(&'a K, &'a V), [], never> for alloc.collections.HashMapIter<'a, K, V>
fn alloc.collections.HashMapIter.next['a, K, V](self: &mut alloc.collections.HashMapIter<'a, K, V>) -> Option<(&'a K, &'a V)> uses [] panics never impls core.iter.Iterator<(&'a K, &'a V), [], never>
fn alloc.collections.VecDeque.is_empty[T, A: alloc.alloc.Allocator](self: &alloc.collections.VecDeque<T, A>) -> bool uses [] panics never pub
fn alloc.collections.VecDeque.len[T, A: alloc.alloc.Allocator](self: &alloc.collections.VecDeque<T, A>) -> usize uses [] panics never pub
fn alloc.collections.VecDeque.new[T, A: alloc.alloc.Allocator](allocator: A) -> alloc.collections.VecDeque<T, A> uses [] panics never pub
fn alloc.collections.VecDeque.pop_back[T, A: alloc.alloc.Allocator](self: &mut alloc.collections.VecDeque<T, A>) -> Option<T> uses [] panics never pub
fn alloc.collections.VecDeque.pop_front[T, A: alloc.alloc.Allocator](self: &mut alloc.collections.VecDeque<T, A>) -> Option<T> uses [] panics never pub
fn alloc.collections.VecDeque.push_back[T, A: alloc.alloc.Allocator](self: &mut alloc.collections.VecDeque<T, A>, value: T) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never pub
fn alloc.collections.VecDeque.push_front[T, A: alloc.alloc.Allocator](self: &mut alloc.collections.VecDeque<T, A>, value: T) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never pub
