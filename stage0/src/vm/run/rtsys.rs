//! `std.task` の実行器が頼る実行時の核の VM 実装 (`sysrt.c` の `ksn_rt_sys_io_*` / `ksn_rt_sys_mt_*` / `ksn_rt_sys_task_session` の移植)。
//! VM は 1 本のスレッドで協調的に走るので、ロック・原子操作は要らない。`block_on_threads` の注入列・完了の印・取り消しの印・眠りと、
//! 非同期ソケットの epoll リアクター (`accept_async` / `read_async` / `write_async` の待ち) をここに持つ。意味は `sysrt.c` と同じ
//! (設計_VM一本化.md 6 章)。

use super::*;
use std::collections::VecDeque;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

/// `ksn.mt.*` の取っ手の印 (`sysrt.c` の `KSN_MT_MARK`)。下位は項目の番号 + 1。
const MARK: u64 = 1 << 62;

struct MtItem {
    fp: u64,
    tag: u32,
    /// 0 = 待ち (注入列)、1 = 走行中、2 = 終わり、3 = 取り消し
    state: u32,
    cancel: u32,
}

/// 複数スレッドの実行器の共有の核 (VM ではワーカーは呼び出しスレッド 1 本だけ)。
#[derive(Default)]
pub(in crate::vm) struct MtCore {
    items: Vec<MtItem>,
    q: VecDeque<usize>,
    unfinished: i64,
    epoch: u64,
    stop: bool,
    pub(in crate::vm) on: bool,
    n: i64,
    idle: i64,
    idle_ep: u64,
    sess: u64,
}

static SESS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn next_sess() -> u64 {
    SESS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
}

#[derive(Clone, Copy, Default)]
struct IoSlot {
    at: Option<Instant>,
    task: i64,
    /// 0 なし / 1 待ち / 2 準備完了 / 3 期限切れ
    st: u8,
    reg: bool,
    dir: usize,
    rdy: [bool; 2],
}

/// 非同期ソケットのリアクター (epoll)。
pub(in crate::vm) struct IoCore {
    ep: i32,
    slots: std::collections::HashMap<i32, IoSlot>,
    nparked: i64,
    nfired: i64,
    fq: VecDeque<i64>,
    tickn: u32,
    dl: Vec<i32>,
}

impl Default for IoCore {
    fn default() -> Self {
        IoCore { ep: -1, slots: Default::default(), nparked: 0, nfired: 0, fq: VecDeque::new(), tickn: 0, dl: vec![] }
    }
}

impl Drop for IoCore {
    fn drop(&mut self) {
        if self.ep >= 0 {
            unsafe { libc::close(self.ep) };
        }
    }
}

impl IoCore {
    /// fd を閉じる前に呼ぶ: 登録の記録を消す (`ksn_rt_sys_io_forget`)。
    pub(in crate::vm) fn forget(&mut self, fd: i32) {
        if let Some(s) = self.slots.remove(&fd) {
            if s.st == 1 {
                self.nparked -= 1;
            }
            if s.st >= 2 {
                self.nparked -= 1;
                self.nfired -= 1;
            }
        }
    }

    fn scan_dl(&mut self, now: Instant, best: &mut Option<Instant>) -> i64 {
        let mut got = 0;
        let list = std::mem::take(&mut self.dl);
        let mut keep = vec![];
        for fd in list {
            let Some(s) = self.slots.get_mut(&fd) else { continue };
            let Some(at) = s.at else { continue };
            if s.st != 1 {
                continue;
            }
            if at <= now {
                s.st = 3;
                got += 1;
                self.nfired += 1;
                if s.task >= 0 {
                    self.fq.push_back(s.task);
                }
                continue;
            }
            if best.map_or(true, |b| at < b) {
                *best = Some(at);
            }
            keep.push(fd);
        }
        self.dl = keep;
        got
    }
}

fn to_instant(d: Duration) -> Instant {
    Instant::now() + d
}

impl<'a> Vm<'a> {
    /// 仮想の記述子 `vfd` の実際の fd。
    fn raw_fd(&self, vfd: i32) -> Option<i32> {
        match self.socks.get(&(vfd as i128))? {
            NetSock::Listener(l) => Some(l.as_raw_fd()),
            NetSock::Stream(s) => Some(s.as_raw_fd()),
            NetSock::Udp(u) => Some(u.as_raw_fd()),
        }
    }

    /// fd の読み / 書きの期限 (`set_*_deadline` の表と、`set_*_timeout` を見る)。
    fn io_deadline(&self, vfd: i32, write: bool) -> Option<Instant> {
        let mut at = self.deadlines.get(&(vfd as i128)).and_then(|d| if write { d.1 } else { d.0 });
        if let Some(NetSock::Stream(s)) = self.socks.get(&(vfd as i128)) {
            let t = if write { s.write_timeout() } else { s.read_timeout() };
            if let Ok(Some(d)) = t {
                let t = to_instant(d);
                if at.map_or(true, |a| t < a) {
                    at = Some(t);
                }
            }
        }
        at
    }

    fn io_park(&mut self, vfd: i32, write: bool) -> u64 {
        let Some(raw) = self.raw_fd(vfd) else { return 0 };
        if self.io.ep < 0 {
            let ep = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
            if ep < 0 {
                return 0;
            }
            self.io.ep = ep;
        }
        let d = write as usize;
        let at = self.io_deadline(vfd, write);
        let ep = self.io.ep;
        let s = self.io.slots.entry(vfd).or_default();
        if !s.reg {
            let mut ev = libc::epoll_event { events: (libc::EPOLLIN | libc::EPOLLOUT | libc::EPOLLRDHUP | libc::EPOLLET) as u32, u64: vfd as u32 as u64 };
            if unsafe { libc::epoll_ctl(ep, libc::EPOLL_CTL_ADD, raw, &mut ev) } != 0 {
                return 0;
            }
            s.reg = true;
        }
        if s.rdy[d] {
            s.rdy[d] = false;
            return 0;
        }
        if s.st == 0 {
            self.io.nparked += 1;
        }
        s.st = 1;
        s.dir = d;
        s.at = at;
        if at.is_some() {
            self.io.dl.push(vfd);
        }
        1
    }

    fn io_poll(&mut self, block: u32) -> u64 {
        if self.io.ep < 0 || self.io.nparked == 0 {
            return 0;
        }
        if block == 0 {
            self.io.tickn = self.io.tickn.wrapping_add(1);
            if self.io.tickn & 7 != 0 {
                return 0;
            }
        }
        let mut got = 0i64;
        let mut timeout: i32 = 0;
        if block != 0 {
            let mut best = None;
            if !self.io.dl.is_empty() {
                got += self.io.scan_dl(Instant::now(), &mut best);
            }
            if got == 0 {
                timeout = match best {
                    None => -1,
                    Some(b) => {
                        let now = Instant::now();
                        if b > now {
                            ((b - now).as_millis() as i32 + 1).max(1)
                        } else {
                            0
                        }
                    }
                };
            }
        }
        let mut evs = [libc::epoll_event { events: 0, u64: 0 }; 256];
        let n = unsafe { libc::epoll_wait(self.io.ep, evs.as_mut_ptr(), 256, timeout) };
        for k in 0..n.max(0) as usize {
            let (e, id) = (evs[k].events, evs[k].u64);
            let fd = id as u32 as i32;
            let Some(s) = self.io.slots.get_mut(&fd) else { continue };
            let hit = [(e & (libc::EPOLLIN | libc::EPOLLRDHUP | libc::EPOLLHUP | libc::EPOLLERR) as u32) != 0, (e & (libc::EPOLLOUT | libc::EPOLLHUP | libc::EPOLLERR) as u32) != 0];
            for d in 0..2 {
                if !hit[d] {
                    continue;
                }
                if s.st == 1 && s.dir == d {
                    s.st = 2;
                    got += 1;
                    self.io.nfired += 1;
                    if s.task >= 0 {
                        self.io.fq.push_back(s.task);
                    }
                } else {
                    s.rdy[d] = true;
                }
            }
        }
        if !self.io.dl.is_empty() && self.io.nparked > self.io.nfired {
            let mut none = None;
            got += self.io.scan_dl(Instant::now(), &mut none);
        }
        got as u64
    }

    /// 実行時の核の呼び出し。引数 `a` `b` は引数のスロット。結果の型が unit のものは `d` に書かない。
    pub(super) fn rt(&mut self, rp: *mut u64, op: Rt, d: S, a: S, b: S) -> Result<(), String> {
        let ra = || unsafe { *rp.add(a as usize) };
        let rb = || unsafe { *rp.add(b as usize) };
        let item = |me: &mut Self, id: u64| -> Result<usize, String> {
            let i = (id & !MARK) as usize;
            if i == 0 || i > me.mt.items.len() {
                return Err(format!("ksn.mt: 不正な取っ手 {:#x}", id));
            }
            Ok(i - 1)
        };
        let mut res: Option<u64> = None;
        match op {
            Rt::MtMarked => res = Some((ra() >= MARK) as u64),
            Rt::MtBegin => {
                let n = (ra() as i64).min(64);
                let m = &mut self.mt;
                m.items.clear();
                m.q.clear();
                m.unfinished = 0;
                m.stop = false;
                m.idle = 0;
                m.idle_ep = 0;
                m.sess = next_sess();
                // VM のワーカーは呼び出しスレッド 1 本だけ (補助のワーカーは起こさない): 全員待ちの数 `n` は 1。
                m.n = 1;
                m.on = n > 1;
                res = Some(if m.on { 1 } else { 0 });
            }
            Rt::MtOn => res = Some(self.mt.on as u64),
            Rt::MtPush => {
                let (tag, fp) = (ra() as u32, rb());
                let m = &mut self.mt;
                m.items.push(MtItem { fp, tag, state: 0, cancel: 0 });
                m.q.push_back(m.items.len() - 1);
                m.unfinished += 1;
                m.epoch += 1;
                res = Some(m.items.len() as u64 | MARK);
            }
            Rt::MtTake => {
                let m = &mut self.mt;
                let mut got = 0;
                while let Some(i) = m.q.pop_front() {
                    if m.items[i].state == 0 {
                        m.items[i].state = 1;
                        got = (i + 1) as u64 | MARK;
                        break;
                    }
                }
                res = Some(got);
            }
            Rt::MtFinish => {
                let (id, cancelled) = (ra(), rb() as u32);
                let i = item(self, id)?;
                let m = &mut self.mt;
                m.items[i].state = if cancelled != 0 { 3 } else { 2 };
                m.unfinished -= 1;
                m.epoch += 1;
            }
            Rt::MtState => {
                let i = item(self, ra())?;
                res = Some(self.mt.items[i].state as u64);
            }
            Rt::MtFp => {
                let i = item(self, ra())?;
                res = Some(self.mt.items[i].fp);
            }
            Rt::MtTag => {
                let i = item(self, ra())?;
                res = Some(self.mt.items[i].tag as u64);
            }
            Rt::MtCancelled => {
                let i = item(self, ra())?;
                res = Some(self.mt.items[i].cancel as u64);
            }
            Rt::MtCancel => {
                let i = item(self, ra())?;
                let m = &mut self.mt;
                let mut r = 0;
                match m.items[i].state {
                    0 => {
                        m.items[i].state = 3;
                        m.unfinished -= 1;
                        r = 1;
                        m.epoch += 1;
                    }
                    1 => {
                        m.items[i].cancel = 1;
                        m.epoch += 1;
                    }
                    _ => {}
                }
                res = Some(r);
            }
            Rt::MtClearFp => {
                let i = item(self, ra())?;
                self.mt.items[i].fp = 0;
            }
            Rt::MtEpoch => res = Some(self.mt.epoch + 1),
            Rt::MtSleep => {
                let seen = ra();
                let m = &mut self.mt;
                let mut r = 0u64;
                if m.epoch + 1 == seen && !m.stop {
                    if m.idle_ep != seen {
                        m.idle = 0;
                        m.idle_ep = seen;
                    }
                    m.idle += 1;
                    // 呼び出しスレッドが唯一のワーカー: 出来事が起きないまま眠れば、二度と起きない (全員待ち)。
                    r = 3;
                }
                if r == 0 && m.stop {
                    r = 2;
                }
                res = Some(r);
            }
            Rt::MtStop => {
                self.mt.stop = true;
                self.mt.epoch += 1;
            }
            Rt::MtStopped => res = Some(self.mt.stop as u64),
            Rt::MtUnfinished => res = Some(self.mt.unfinished as u64),
            Rt::MtReg => {}
            Rt::MtNthr => res = Some(0),
            Rt::MtThr => res = Some(0),
            Rt::MtEnd => {
                self.mt.items.clear();
                self.mt.q.clear();
                self.mt.on = false;
            }
            Rt::Session => {
                if self.mt.on {
                    res = Some(self.mt.sess);
                } else {
                    // 非 mt: KIR の本体と同じ (実行器の状態 `ksn.task.g` の +48 の世代に 1 足す)。
                    let g = self.prog_global("ksn.task.g").ok_or("ksn.task.g がない")?;
                    let p = addr(g + 48, 8)? as *const u64;
                    res = Some(unsafe { *p } + 1);
                }
            }
            Rt::IoPark => res = Some(self.io_park(ra() as u32 as i32, rb() as u32 != 0)),
            Rt::IoPoll => res = Some(self.io_poll(ra() as u32)),
            Rt::IoReady => {
                let fd = ra() as u32 as i32;
                res = Some(self.io.slots.get(&fd).map_or(false, |s| s.st >= 2) as u64);
            }
            Rt::IoTake => {
                let fd = ra() as u32 as i32;
                let mut r = 0;
                if let Some(s) = self.io.slots.get_mut(&fd) {
                    if s.st != 0 {
                        r = (s.st == 3) as u64;
                        if s.st >= 2 {
                            self.io.nfired -= 1;
                        }
                        self.io.nparked -= 1;
                        s.st = 0;
                    }
                }
                res = Some(r);
            }
            Rt::IoBind => {
                let fd = ra() as u32 as i32;
                if let Some(s) = self.io.slots.get_mut(&fd) {
                    s.task = rb() as i64;
                }
            }
            Rt::IoNfired => res = Some(!self.io.fq.is_empty() as u64),
            Rt::IoNext => res = Some(self.io.fq.pop_front().unwrap_or(0) as u64),
            Rt::IoPending => res = Some((self.io.nparked > 0) as u64),
        }
        if let Some(v) = res {
            unsafe { *rp.add(d as usize) = v };
        }
        Ok(())
    }

    fn prog_global(&self, name: &str) -> Option<u64> {
        self.prog.globals.get(name).map(|p| *p as u64)
    }
}
