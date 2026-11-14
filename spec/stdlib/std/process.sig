type std.process.Child linear pub
    pid: i32
    stdin: i32
    stdout: i32
    stderr: i32
end std.process.Child

type std.process.Command linear pub
    handle: i32
end std.process.Command

type std.process.ExitStatus derive [Debug, Clone, Copy, PartialEq, Eq] pub
    code: i32 pub
    signal: i32 pub
end std.process.ExitStatus

fn std.process.Child.close_stdin(self: &mut std.process.Child) -> Result<(), std.io.Error> uses [process] panics never pub
fn std.process.Child.id(self: &std.process.Child) -> i32 uses [process] panics never pub
fn std.process.Child.kill(self: &mut std.process.Child) -> Result<(), std.io.Error> uses [process] panics never pub
fn std.process.Child.read_stderr(self: &mut std.process.Child, buffer: &mut [u8]) -> Result<usize, std.io.Error> uses [process] panics never pub
fn std.process.Child.read_stdout(self: &mut std.process.Child, buffer: &mut [u8]) -> Result<usize, std.io.Error> uses [process] panics never pub
fn std.process.Child.wait(self: std.process.Child) -> Result<std.process.ExitStatus, std.io.Error> uses [process] panics never pub
fn std.process.Child.write_stdin(self: &mut std.process.Child, buffer: &[u8]) -> Result<usize, std.io.Error> uses [process] panics never pub
fn std.process.Command.arg(self: &mut std.process.Command, argument: &str) -> () uses [process] panics never pub
fn std.process.Command.cancel(self: std.process.Command) -> () uses [process] panics never pub
fn std.process.Command.current_dir(self: &mut std.process.Command, path: &str) -> () uses [process] panics never pub
fn std.process.Command.env(self: &mut std.process.Command, key: &str, value: &str) -> () uses [process] panics never pub
fn std.process.Command.env_clear(self: &mut std.process.Command) -> () uses [process] panics never pub
fn std.process.Command.env_remove(self: &mut std.process.Command, key: &str) -> () uses [process] panics never pub
fn std.process.Command.new(program: &str) -> std.process.Command uses [process] panics never pub
fn std.process.Command.output[A: alloc.alloc.Allocator](self: std.process.Command, out: &mut alloc.vec.Vec<u8, A>, err: &mut alloc.vec.Vec<u8, A>) -> Result<std.process.ExitStatus, std.io.Error> uses [alloc, process] panics never pub
fn std.process.Command.spawn(self: std.process.Command) -> Result<std.process.Child, std.io.Error> uses [process] panics never pub
fn std.process.Command.stderr_null(self: &mut std.process.Command) -> () uses [process] panics never pub
fn std.process.Command.stderr_piped(self: &mut std.process.Command) -> () uses [process] panics never pub
fn std.process.Command.stdin_null(self: &mut std.process.Command) -> () uses [process] panics never pub
fn std.process.Command.stdin_piped(self: &mut std.process.Command) -> () uses [process] panics never pub
fn std.process.Command.stdout_null(self: &mut std.process.Command) -> () uses [process] panics never pub
fn std.process.Command.stdout_piped(self: &mut std.process.Command) -> () uses [process] panics never pub
fn std.process.abort() -> ! uses [process] panics never pub
fn std.process.exit(code: i32) -> ! uses [process] panics never pub
