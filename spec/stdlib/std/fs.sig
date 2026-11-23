type std.fs.Dir linear pub
    descriptor: i32
end std.fs.Dir

type std.fs.File linear pub
    descriptor: i32
end std.fs.File

fn std.fs.Dir.close(self: std.fs.Dir) -> Result<(), std.io.Error> uses [io.fs] panics never pub
fn std.fs.Dir.open(path: &str) -> Result<std.fs.Dir, std.io.Error> uses [io.fs] panics never pub
fn std.fs.Dir.open_file_beneath(self: &std.fs.Dir, rel: &str) -> Result<std.fs.File, std.io.Error> uses [io.fs] panics never pub
fn std.fs.File.close(self: std.fs.File) -> Result<(), std.io.Error> uses [io.fs] panics never pub
impl std.io.Write<[io.fs]> for std.fs.File
fn std.fs.File.flush(self: &mut std.fs.File) -> Result<(), std.io.Error> uses [io.fs] panics never impls std.io.Write<[io.fs]>
impl std.io.Read<[io.fs]> for std.fs.File
fn std.fs.File.read(self: &mut std.fs.File, buffer: &mut [u8]) -> Result<usize, std.io.Error> uses [io.fs] panics never impls std.io.Read<[io.fs]>
fn std.fs.File.sync(self: &mut std.fs.File) -> Result<(), std.io.Error> uses [io.fs] panics never pub
fn std.fs.File.write(self: &mut std.fs.File, buffer: &[u8]) -> Result<usize, std.io.Error> uses [io.fs] panics never impls std.io.Write<[io.fs]>
fn std.fs.create(path: &str) -> Result<std.fs.File, std.io.Error> uses [io.fs] panics never pub
fn std.fs.create_dir(path: &str) -> Result<(), std.io.Error> uses [io.fs] panics never pub
fn std.fs.exists(path: &str) -> bool uses [io.fs] panics never pub
fn std.fs.open(path: &str) -> Result<std.fs.File, std.io.Error> uses [io.fs] panics never pub
fn std.fs.open_append(path: &str) -> Result<std.fs.File, std.io.Error> uses [io.fs] panics never pub
fn std.fs.open_beneath_raw(dir: &std.fs.Dir, rel: &str) -> Result<std.fs.File, std.io.Error> uses [io.fs] panics never pub
fn std.fs.open_dir_raw(path: &str) -> Result<std.fs.Dir, std.io.Error> uses [io.fs] panics never pub
fn std.fs.read_dir_raw[A: alloc.alloc.Allocator](path: &str, out: &mut alloc.vec.Vec<u8, A>) -> Result<usize, std.io.Error> uses [alloc, io.fs] panics never pub
fn std.fs.read_to_end[A: alloc.alloc.Allocator](file: &mut std.fs.File, out: &mut alloc.vec.Vec<u8, A>) -> Result<usize, std.io.Error> uses [alloc, io.fs] panics never pub
fn std.fs.remove_dir(path: &str) -> Result<(), std.io.Error> uses [io.fs] panics never pub
fn std.fs.remove_file(path: &str) -> Result<(), std.io.Error> uses [io.fs] panics never pub
fn std.fs.rename(from: &str, to: &str) -> Result<(), std.io.Error> uses [io.fs] panics never pub
fn std.fs.stat_fd_raw(file: &std.fs.File, field: u32) -> Result<u64, std.io.Error> uses [io.fs] panics never pub
fn std.fs.stat_raw(path: &str, field: u32) -> Result<u64, std.io.Error> uses [io.fs] panics never pub
