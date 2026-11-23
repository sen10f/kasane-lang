type std.net.Error derive [Debug, Clone, Copy, PartialEq, Eq] pub
    code: i32 pub
end std.net.Error

trait std.net.Handler[E: uses, P: panics] pub
    fn handle(self: &Self, request: &[u8]) -> Result<(), std.net.Error> uses [E] panics P
end std.net.Handler

enum std.net.Shutdown derive [Debug, Clone, Copy, PartialEq, Eq] pub
    Read
    Write
    Both
end std.net.Shutdown

type std.net.TcpListener linear pub
    descriptor: i32
end std.net.TcpListener

type std.net.TcpStream linear pub
    descriptor: i32
end std.net.TcpStream

type std.net.UdpSocket linear pub
    descriptor: i32
end std.net.UdpSocket

fn std.net.TcpListener.accept(self: &std.net.TcpListener) -> Result<std.net.TcpStream, std.net.Error> uses [io.net] panics never pub
fn std.net.TcpListener.accept_timeout(self: &std.net.TcpListener, ms: u64) -> Result<std.net.TcpStream, std.net.Error> uses [io.net] panics never pub
fn std.net.TcpListener.close(self: std.net.TcpListener) -> Result<(), std.net.Error> uses [io.net] panics never pub
fn std.net.TcpListener.local_port(self: &std.net.TcpListener) -> Result<u16, std.net.Error> uses [io.net] panics never pub
fn std.net.TcpStream.clear_deadlines(self: &std.net.TcpStream) -> Result<(), std.io.Error> uses [io.net] panics never pub
fn std.net.TcpStream.close(self: std.net.TcpStream) -> Result<(), std.net.Error> uses [io.net] panics never pub
fn std.net.TcpStream.fd(self: &std.net.TcpStream) -> i32 uses [] panics never pub
impl std.io.Write<[io.net]> for std.net.TcpStream
fn std.net.TcpStream.flush(self: &mut std.net.TcpStream) -> Result<(), std.io.Error> uses [io.net] panics never impls std.io.Write<[io.net]>
fn std.net.TcpStream.local_addr_raw(self: &std.net.TcpStream, out: &mut [u8]) -> Result<usize, std.io.Error> uses [io.net] panics never pub
fn std.net.TcpStream.peer_addr_raw(self: &std.net.TcpStream, out: &mut [u8]) -> Result<usize, std.io.Error> uses [io.net] panics never pub
impl std.io.Read<[io.net]> for std.net.TcpStream
fn std.net.TcpStream.read(self: &mut std.net.TcpStream, buffer: &mut [u8]) -> Result<usize, std.io.Error> uses [io.net] panics never impls std.io.Read<[io.net]>
fn std.net.TcpStream.set_nodelay(self: &std.net.TcpStream, on: bool) -> Result<(), std.io.Error> uses [io.net] panics never pub
fn std.net.TcpStream.set_read_deadline(self: &std.net.TcpStream, ms_from_now: u64) -> Result<(), std.io.Error> uses [io.net] panics never pub
fn std.net.TcpStream.set_read_timeout(self: &std.net.TcpStream, millis: u64) -> Result<(), std.io.Error> uses [io.net] panics never pub
fn std.net.TcpStream.set_write_deadline(self: &std.net.TcpStream, ms_from_now: u64) -> Result<(), std.io.Error> uses [io.net] panics never pub
fn std.net.TcpStream.set_write_timeout(self: &std.net.TcpStream, millis: u64) -> Result<(), std.io.Error> uses [io.net] panics never pub
fn std.net.TcpStream.shutdown(self: &std.net.TcpStream, how: std.net.Shutdown) -> Result<(), std.io.Error> uses [io.net] panics never pub
fn std.net.TcpStream.write(self: &mut std.net.TcpStream, buffer: &[u8]) -> Result<usize, std.io.Error> uses [io.net] panics never impls std.io.Write<[io.net]>
fn std.net.UdpSocket.bind(address: &str) -> Result<std.net.UdpSocket, std.net.Error> uses [io.net] panics never pub
fn std.net.UdpSocket.close(self: std.net.UdpSocket) -> Result<(), std.net.Error> uses [io.net] panics never pub
fn std.net.UdpSocket.local_port(self: &std.net.UdpSocket) -> Result<u16, std.net.Error> uses [io.net] panics never pub
fn std.net.UdpSocket.recv_from_raw(self: &std.net.UdpSocket, buffer: &mut [u8], from: &mut [u8]) -> Result<usize, std.io.Error> uses [io.net] panics never pub
fn std.net.UdpSocket.send_to(self: &std.net.UdpSocket, buffer: &[u8], address: &str) -> Result<usize, std.io.Error> uses [io.net] panics never pub
fn std.net.connect(address: &str) -> Result<std.net.TcpStream, std.net.Error> uses [io.net] panics never pub
fn std.net.connect_timeout(address: &str, ms: u64) -> Result<std.net.TcpStream, std.net.Error> uses [io.net] panics never pub
fn std.net.listen(address: &str) -> Result<std.net.TcpListener, std.net.Error> uses [io.net] panics never pub
fn std.net.resolve_raw(host: &str, port: u16, out: &mut [u8]) -> Result<usize, std.io.Error> uses [io.net] panics never pub
