type std.io.Error derive [Debug, Clone, Copy, PartialEq, Eq] pub
    kind: std.io.ErrorKind pub
    code: i32 pub
end std.io.Error

enum std.io.ErrorKind derive [Debug, Clone, Copy, PartialEq, Eq] pub
    NotFound
    PermissionDenied
    AlreadyExists
    InvalidInput
    UnexpectedEof
    BrokenPipe
    Interrupted
    OutOfMemory
    Other
end std.io.ErrorKind

trait std.io.Read[E: uses] pub
    fn read(self: &mut Self, buffer: &mut [u8]) -> Result<usize, std.io.Error> uses [E] panics never
end std.io.Read

type std.io.Stderr pub
    descriptor: i32
end std.io.Stderr

type std.io.Stdin pub
    descriptor: i32
end std.io.Stdin

type std.io.Stdout pub
    descriptor: i32
end std.io.Stdout

trait std.io.Write[E: uses] pub
    fn flush(self: &mut Self) -> Result<(), std.io.Error> uses [E] panics never
    fn write(self: &mut Self, buffer: &[u8]) -> Result<usize, std.io.Error> uses [E] panics never
end std.io.Write

impl std.io.Write<[io.console]> for std.io.Stderr
fn std.io.Stderr.flush(self: &mut std.io.Stderr) -> Result<(), std.io.Error> uses [io.console] panics never impls std.io.Write<[io.console]>
fn std.io.Stderr.write(self: &mut std.io.Stderr, buffer: &[u8]) -> Result<usize, std.io.Error> uses [io.console] panics never impls std.io.Write<[io.console]>
impl std.io.Read<[io.console]> for std.io.Stdin
fn std.io.Stdin.read(self: &mut std.io.Stdin, buffer: &mut [u8]) -> Result<usize, std.io.Error> uses [io.console] panics never impls std.io.Read<[io.console]>
impl std.io.Write<[io.console]> for std.io.Stdout
fn std.io.Stdout.flush(self: &mut std.io.Stdout) -> Result<(), std.io.Error> uses [io.console] panics never impls std.io.Write<[io.console]>
fn std.io.Stdout.write(self: &mut std.io.Stdout, buffer: &[u8]) -> Result<usize, std.io.Error> uses [io.console] panics never impls std.io.Write<[io.console]>
fn std.io.print_line(text: &str) -> Result<(), std.io.Error> uses [io.console] panics never pub
fn std.io.stderr() -> std.io.Stderr uses [] panics never pub
fn std.io.stdin() -> std.io.Stdin uses [] panics never pub
fn std.io.stdout() -> std.io.Stdout uses [] panics never pub
