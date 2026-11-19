enum alloc.string.PushBytesError pub
    Alloc(alloc.alloc.AllocError)
    InvalidUtf8(core.str.Utf8Error)
end alloc.string.PushBytesError

type alloc.string.String[A: alloc.alloc.Allocator] pub
    buf: alloc.raw.Buf<u8, A>
end alloc.string.String

impl[A: alloc.alloc.Allocator] core.cmp.Eq for alloc.string.String<A>

// 組み込み (KIR の `hdr_view`)。検査を省く O(1)。バイト列は非公開で、書くのは UTF-8 を保つ `String` のメソッドだけ (8.3)。
fn alloc.string.String.as_str[A: alloc.alloc.Allocator](self: &alloc.string.String<A>) -> &str uses [] panics never pub
impl[A: alloc.alloc.Allocator] core.cmp.Ord for alloc.string.String<A>
fn alloc.string.String.cmp[A: alloc.alloc.Allocator](self: &alloc.string.String<A>, other: &alloc.string.String<A>) -> core.cmp.Ordering uses [] panics never impls core.cmp.Ord
impl[A: alloc.alloc.Allocator] core.fmt.Debug for alloc.string.String<A>
fn alloc.string.String.debug_fmt[A: alloc.alloc.Allocator](self: &alloc.string.String<A>, f: &mut core.fmt.Formatter) -> Result<(), core.fmt.Error> uses [] panics never impls core.fmt.Debug
impl[A: alloc.alloc.Allocator] core.fmt.Display for alloc.string.String<A>
fn alloc.string.String.display_fmt[A: alloc.alloc.Allocator](self: &alloc.string.String<A>, f: &mut core.fmt.Formatter) -> Result<(), core.fmt.Error> uses [] panics never impls core.fmt.Display
impl[A: alloc.alloc.Allocator] core.cmp.PartialEq<alloc.string.String<A>> for alloc.string.String<A>
fn alloc.string.String.eq[A: alloc.alloc.Allocator](self: &alloc.string.String<A>, other: &alloc.string.String<A>) -> bool uses [] panics never impls core.cmp.PartialEq<alloc.string.String<A>>
fn alloc.string.String.from_str[A: alloc.alloc.Allocator](allocator: A, text: &str) -> Result<alloc.string.String<A>, alloc.alloc.AllocError> uses [alloc] panics never pub
impl[A: alloc.alloc.Allocator] core.hash.Hash for alloc.string.String<A>
fn alloc.string.String.hash[A: alloc.alloc.Allocator, H: core.hash.Hasher](self: &alloc.string.String<A>, state: &mut H) -> () uses [] panics never impls core.hash.Hash
fn alloc.string.String.new[A: alloc.alloc.Allocator](allocator: A) -> alloc.string.String<A> uses [] panics never pub
impl[A: alloc.alloc.Allocator] core.cmp.PartialOrd<alloc.string.String<A>> for alloc.string.String<A>
fn alloc.string.String.partial_cmp[A: alloc.alloc.Allocator](self: &alloc.string.String<A>, other: &alloc.string.String<A>) -> Option<core.cmp.Ordering> uses [] panics never impls core.cmp.PartialOrd<alloc.string.String<A>>
fn alloc.string.String.push_debug[A: alloc.alloc.Allocator, T: core.fmt.Debug](self: &mut alloc.string.String<A>, value: &T) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never pub
fn alloc.string.String.push_display[A: alloc.alloc.Allocator, T: core.fmt.Display](self: &mut alloc.string.String<A>, value: &T) -> Result<(), alloc.alloc.AllocError> uses [alloc] panics never pub
impl[A: alloc.alloc.Allocator + core.ops.Clone] alloc.alloc.TryClone for alloc.string.String<A>
fn alloc.string.String.try_clone[A: alloc.alloc.Allocator + core.ops.Clone](self: &alloc.string.String<A>) -> Result<alloc.string.String<A>, alloc.alloc.AllocError> uses [alloc] panics never impls alloc.alloc.TryClone pub
