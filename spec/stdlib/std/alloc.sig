const std.alloc.SYSTEM: std.alloc.System = std.alloc.System { id: 0u8 } pub

type std.alloc.System derive [Debug, Clone, Copy] pub
    id: u8
end std.alloc.System

impl alloc.alloc.Allocator for std.alloc.System
fn std.alloc.System.alloc(self: &std.alloc.System, fit: alloc.alloc.Layout) -> Result<*mut u8, alloc.alloc.AllocError> uses [alloc] panics never impls alloc.alloc.Allocator
fn std.alloc.System.dealloc(self: &std.alloc.System, pointer: *mut u8, fit: alloc.alloc.Layout) -> () uses [] panics never impls alloc.alloc.Allocator
fn std.alloc.System.realloc(self: &std.alloc.System, pointer: *mut u8, old: alloc.alloc.Layout, new_size: usize) -> Result<*mut u8, alloc.alloc.AllocError> uses [alloc] panics never impls alloc.alloc.Allocator
