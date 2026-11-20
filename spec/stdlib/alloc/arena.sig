// 世代付きの取っ手で要素を借りる `Arena` (8.3)。`Handle<T>` は `Arena<T, A>` の中の要素の位置
// (添字と世代) だけを持つ `Copy` の値で、作れるのは `Arena` の側だけ (欄は非公開)。
// `remove` した要素の取っ手は「弱い」参照になり、`get` は `None` を返す (世代を 1 つ上げるので、
// 再利用したスロットの古い取っ手も当たらない)。`T: linear` の値は入れられない (線形コンテナ規則)。
// shortcut: 取っ手は (添字, 世代) だけなので、型は同じだが別の `Arena` の、添字と世代が両方一致する
// 取っ手は当たってしまう (世代は `remove` のたびに 1 つ上がるだけ)。防ぐには取っ手に arena 識別の
// 番号を要する。型の違う取っ手は型検査 (E2023) で拒ばれる。
// 要素は `alloc.raw.Buf` のスロットに持つ (`Vec` と同じ 1 本の Buf。チャンクは切らないので、
// 要素の添字は `grow` で動かず、取っ手は値を借りている間も有効)。空きスロットは単方向の連結
// リスト (`free`) で持ち、世代を 1 つ上げて O(1) で再利用する。空きリストの末尾は
// `core.num.usize.MAX`。
type alloc.arena.Arena[T, A: alloc.alloc.Allocator] pub
    slots: alloc.raw.Buf<alloc.arena.Slot<T>, A>
    free: usize
    live: usize
end alloc.arena.Arena

type alloc.arena.Handle[T] derive [Debug, PartialEq, Eq, Hash] pub
    index: usize
    generation: u32
end alloc.arena.Handle

enum alloc.arena.Slot[T] pub
    Free(u32, usize)
    Taken(u32, T)
end alloc.arena.Slot

impl[T: core.marker.Send, A: alloc.alloc.Allocator + core.marker.Send] core.marker.Send for alloc.arena.Arena<T, A>
impl[T: core.marker.Sync, A: alloc.alloc.Allocator + core.marker.Sync] core.marker.Sync for alloc.arena.Arena<T, A>
impl[T] core.ops.Copy for alloc.arena.Handle<T>
