type std.net.Error derive [Debug, Clone, Copy, PartialEq, Eq] pub
    code: i32
end std.net.Error

trait std.net.Handler[E: uses, P: panics] pub
    fn handle(self: &Self, request: &[u8]) -> Result<(), std.net.Error> uses [E] panics P
end std.net.Handler

type std.net.TcpListener linear pub
    descriptor: i32
end std.net.TcpListener

type std.net.TcpStream linear pub
    descriptor: i32
end std.net.TcpStream

fn std.net.TcpListener.accept(self: &std.net.TcpListener) -> Result<std.net.TcpStream, std.net.Error> uses [io.net] panics never pub
fn std.net.TcpListener.close(self: std.net.TcpListener) -> Result<(), std.net.Error> uses [io.net] panics never pub
fn std.net.TcpListener.local_port(self: &std.net.TcpListener) -> Result<u16, std.net.Error> uses [io.net] panics never pub
fn std.net.TcpStream.close(self: std.net.TcpStream) -> Result<(), std.net.Error> uses [io.net] panics never pub
impl std.io.Write<[io.net]> for std.net.TcpStream
fn std.net.TcpStream.flush(self: &mut std.net.TcpStream) -> Result<(), std.io.Error> uses [io.net] panics never impls std.io.Write<[io.net]>
impl std.io.Read<[io.net]> for std.net.TcpStream
fn std.net.TcpStream.read(self: &mut std.net.TcpStream, buffer: &mut [u8]) -> Result<usize, std.io.Error> uses [io.net] panics never impls std.io.Read<[io.net]>
fn std.net.TcpStream.write(self: &mut std.net.TcpStream, buffer: &[u8]) -> Result<usize, std.io.Error> uses [io.net] panics never impls std.io.Write<[io.net]>
fn std.net.connect(address: &str) -> Result<std.net.TcpStream, std.net.Error> uses [io.net] panics never pub
fn std.net.listen(address: &str) -> Result<std.net.TcpListener, std.net.Error> uses [io.net] panics never pub
