//! `block_on_threads` の協調ワーカー (設計_VM一本化.md 6 章の改訂)。ワーカーを OS スレッドではなく VM 内の協調スレッド (コルーチン)
//! にする: それぞれが自分のフレーム列・レジスタ・領域・スレッドごとの大域 (`ksn.task.*`) を持ち、待ち (sleep・空のチャネルの recv・満杯の
//! send・持たれた Mutex・join) と一定回数ごとの回転 (Mutex の取得・原子操作・時計) で別のワーカーへ切り替える。ネイティブの「ほかのワーカーが
//! 進む」を、1 本の OS スレッドの上で同じ順序の制約 (切り替えは切り替え点だけ) で再現する。全員が待つと deadlock (101)。

use super::*;
use std::time::Instant;

/// 切り替え後の位置 (版, 関数, 命令, フレームの底)。
pub(in crate::vm) type Cur = (u32, u32, u32, u32);

#[derive(Clone, Copy, Debug)]
pub(in crate::vm) enum CoSt {
    Run,
    Sleep(Instant),
    /// 状態が変わる (`progress` が進む) まで待つ。
    Poll(u64),
    /// 実行器の眠り (`ksn.mt.sleep(seen)`)。世代が進むか止めの旗まで。
    MtIdle(u64),
    /// スレッド項目 `idx` のワーカーの終わりまで。
    Join(usize),
    Done,
}

pub(in crate::vm) struct Co {
    regs: Vec<u64>,
    frames: Vec<Frame>,
    allocs: Vec<*mut std::ffi::c_void>,
    top: usize,
    cur: Cur,
    tls: Vec<Vec<u8>>,
    st: CoSt,
    pub(in crate::vm) idx: usize,
    /// `MtSleep` で数え済みの世代。
    pub(in crate::vm) idle_seen: u64,
}

/// 切り替えのあとの再開位置 (切り替え点の命令を `Sched` とする)。
pub(super) enum Res {
    /// `Sched` を実行し直す (条件を見直す)。
    Rewind,
    /// 直後の呼び出しを実行する。
    Here,
    /// 直後の呼び出しを飛ばす。
    After,
}

pub(super) enum SchedOut {
    Go,
    Skip,
    Sw(CoSt, Res),
}

/// ワーカー入口の鍵の FNV-1a (`kir_lower_thread::tag_of("ksn.task.worker")`)。
fn worker_tag() -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in "ksn.task.worker".bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// スレッド表 `ksn.thr` の 1 項目の大きさ (`kir_lower_thread.rs` の `ESZ`)。
const ESZ: usize = 24 + 64 + 64;

impl<'a> Vm<'a> {
    /// 協調ワーカーが 2 本以上いて、切り替えてよい (C のコールバックの中ではない) か。
    pub(super) fn coop(&self) -> bool {
        self.mt.on && self.cos.len() > 1 && self.nested == 0
    }

    fn co_done(&self, idx: usize) -> Option<bool> {
        self.cos.iter().find(|c| c.idx == idx && c.idx != usize::MAX).map(|c| matches!(c.st, CoSt::Done))
    }

    fn ent_tag(&self, idx: usize) -> Option<u64> {
        let g = *self.prog.globals.get("ksn.thr")?;
        let p = addr((g + 8 + idx * ESZ + 8) as u64, 8).ok()? as *const u64;
        Some(unsafe { *p })
    }

    /// 切り替え点 `k` の処理。`a` は引数のスロット。
    pub(super) fn sched(&mut self, rp: *mut u64, k: SchedK, a: S) -> Result<SchedOut, String> {
        let ra = || unsafe { *rp.add(a as usize) };
        match k {
            SchedK::Prog => {
                self.progress += 1;
                Ok(SchedOut::Go)
            }
            SchedK::Pre => {
                if self.coop() {
                    self.pre += 1;
                    if self.pre & 63 == 0 {
                        return Ok(SchedOut::Sw(CoSt::Run, Res::Here));
                    }
                }
                Ok(SchedOut::Go)
            }
            SchedK::Start => {
                if self.mt.on && self.nested == 0 {
                    self.co_init();
                    let idx = ra() as usize;
                    if self.ent_tag(idx) == Some(worker_tag()) {
                        let ex = self.prog.by_key.get("ksn.thr.exec").copied().ok_or("ksn.thr.exec がない")?;
                        let vi = (self.vers.len() - 1) as u32;
                        let mut regs = vec![0u64; 1 << 14];
                        regs[0] = idx as u64;
                        let tls = self.prog.tls.iter().map(|(_, n)| vec![0u8; *n]).collect();
                        self.cos.push(Co { regs, frames: vec![], allocs: vec![], top: 0, cur: (vi, ex, 0, 0), tls, st: CoSt::Run, idx, idle_seen: 0 });
                        return Ok(SchedOut::Skip);
                    }
                }
                Ok(SchedOut::Go)
            }
            SchedK::Join => {
                if self.nested != 0 {
                    return Ok(SchedOut::Go);
                }
                match self.co_done(ra() as usize) {
                    None => Ok(SchedOut::Go),
                    Some(true) => Ok(SchedOut::Skip),
                    Some(false) => {
                        if std::mem::take(&mut self.giveup) {
                            return Ok(SchedOut::Go);
                        }
                        Ok(SchedOut::Sw(CoSt::Join(ra() as usize), Res::Rewind))
                    }
                }
            }
            SchedK::Sleep => {
                if !self.coop() {
                    return Ok(SchedOut::Go);
                }
                let ms = ra();
                if ms == 0 {
                    Ok(SchedOut::Sw(CoSt::Run, Res::After))
                } else {
                    Ok(SchedOut::Sw(CoSt::Sleep(Instant::now() + std::time::Duration::from_millis(ms)), Res::After))
                }
            }
            SchedK::Wait => {
                if !self.coop() || std::mem::take(&mut self.giveup) {
                    return Ok(SchedOut::Go);
                }
                Ok(SchedOut::Sw(CoSt::Poll(self.progress), Res::After))
            }
            SchedK::MLock | SchedK::RwRd | SchedK::RwWr => {
                if !self.coop() {
                    return Ok(SchedOut::Go);
                }
                let w = unsafe { *(addr(ra(), 4)? as *const u32) };
                let held = match k {
                    SchedK::MLock => w != 0,
                    SchedK::RwRd => w == 0x8000_0000,
                    _ => w != 0,
                };
                if !held {
                    self.pre += 1;
                    if self.pre & 15 == 0 {
                        return Ok(SchedOut::Sw(CoSt::Run, Res::Here));
                    }
                    return Ok(SchedOut::Go);
                }
                if std::mem::take(&mut self.giveup) {
                    return Ok(SchedOut::Go);
                }
                Ok(SchedOut::Sw(CoSt::Poll(self.progress), Res::Rewind))
            }
        }
    }

    /// 最も近い眠りの期限。
    pub(super) fn earliest_sleep(&self) -> Option<Instant> {
        self.cos.iter().enumerate().filter(|(i, _)| *i != self.cur_co).filter_map(|(_, c)| if let CoSt::Sleep(t) = c.st { Some(t) } else { None }).min()
    }

    /// 自分以外に走れるワーカーがいるか。
    pub(super) fn others_runnable(&self) -> bool {
        let now = Instant::now();
        self.cos.iter().enumerate().any(|(i, c)| i != self.cur_co && self.ready(c, now))
    }

    fn ready(&self, c: &Co, now: Instant) -> bool {
        match c.st {
            CoSt::Run => true,
            CoSt::Sleep(t) => t <= now,
            CoSt::Poll(s) => s != self.progress,
            CoSt::MtIdle(s) => self.mt.epoch() + 1 != s || self.mt.stopped(),
            CoSt::Join(j) => self.co_done(j) == Some(true),
            CoSt::Done => false,
        }
    }

    /// 今のワーカーを `st` で止めて次のワーカーへ切り替え、その再開位置を返す。
    pub(super) fn co_switch(&mut self, st: CoSt, cur: Cur) -> Result<Cur, String> {
        let me = self.cur_co;
        let n = self.cos.len();
        // 次を選ぶ。
        self.cos[me].st = st;
        let next = loop {
            let now = Instant::now();
            let mut pick = None;
            for k in 1..=n {
                let i = (me + k) % n;
                if self.ready(&self.cos[i], now) {
                    pick = Some(i);
                    break;
                }
            }
            if let Some(i) = pick {
                if matches!(self.cos[i].st, CoSt::Sleep(_)) {
                    self.progress += 1;
                }
                break i;
            }
            // 走れる者がいない: 眠りの期限まで待つ (I/O の待ちがあれば epoll で待つ)。
            let sl = self.cos.iter().filter_map(|c| if let CoSt::Sleep(t) = c.st { Some(t) } else { None }).min();
            if let Some(t) = sl {
                let d = t.saturating_duration_since(now);
                if self.io_pending_any() {
                    self.io_poll_timeout(d);
                } else {
                    std::thread::sleep(d);
                }
                continue;
            }
            if self.io_pending_any() {
                self.io_poll_timeout(std::time::Duration::from_millis(50));
                continue;
            }
            // 全員が待つ: deadlock。実行器の眠りがあればそれに全員待ちを知らせ、なければ待っている者に本来の呼び出し (panic する) を実行させる。
            let i = (1..=n).map(|k| (me + k) % n).find(|&i| matches!(self.cos[i].st, CoSt::MtIdle(_))).or_else(|| (1..=n).map(|k| (me + k) % n).find(|&i| matches!(self.cos[i].st, CoSt::Poll(_) | CoSt::Join(_))));
            let Some(i) = i else { return Err("協調ワーカー: 走れる者がいない".into()) };
            self.giveup = true;
            break i;
        };
        if next == me {
            self.cos[me].st = CoSt::Run;
            return Ok(cur);
        }
        // 保存して読み込む。
        {
            let c = &mut self.cos[me];
            c.cur = cur;
            std::mem::swap(&mut c.regs, &mut self.regs);
            std::mem::swap(&mut c.frames, &mut self.frames);
            std::mem::swap(&mut c.allocs, &mut self.allocs);
            std::mem::swap(&mut c.top, &mut self.top);
            for (k, (ad, sz)) in self.prog.tls.iter().enumerate() {
                c.tls[k].clear();
                c.tls[k].extend_from_slice(unsafe { std::slice::from_raw_parts(*ad as *const u8, *sz) });
            }
        }
        {
            let c = &mut self.cos[next];
            std::mem::swap(&mut c.regs, &mut self.regs);
            std::mem::swap(&mut c.frames, &mut self.frames);
            std::mem::swap(&mut c.allocs, &mut self.allocs);
            std::mem::swap(&mut c.top, &mut self.top);
            for (k, (ad, sz)) in self.prog.tls.iter().enumerate() {
                if c.tls[k].len() == *sz {
                    unsafe { std::ptr::copy_nonoverlapping(c.tls[k].as_ptr(), *ad as *mut u8, *sz) };
                }
            }
            c.st = CoSt::Run;
        }
        self.cur_co = next;
        Ok(self.cos[next].cur)
    }

    /// 今のワーカーの終わり (入口の関数が戻った)。次へ切り替える (戻らない)。
    pub(super) fn co_finish(&mut self, cur: Cur) -> Result<Cur, String> {
        self.progress += 1;
        self.co_switch(CoSt::Done, cur)
    }

    pub(super) fn co_init(&mut self) {
        if self.cos.is_empty() {
            self.cos.push(Co { regs: vec![], frames: vec![], allocs: vec![], top: 0, cur: (0, 0, 0, 0), tls: self.prog.tls.iter().map(|(_, n)| vec![0u8; *n]).collect(), st: CoSt::Run, idx: usize::MAX, idle_seen: 0 });
        }
    }

    /// 実行器の眠り (`ksn.mt.sleep(seen)`) の協調版。0 = 起きた、2 = 止め、3 = 全員待ち、`None` = 眠る (命令を実行し直す)。
    pub(super) fn mt_sleep(&mut self, seen: u64) -> Option<u64> {
        if self.mt.epoch() + 1 != seen || self.mt.stopped() {
            return Some(if self.mt.stopped() { 2 } else { 0 });
        }
        if std::mem::take(&mut self.giveup) {
            return Some(3);
        }
        let me = self.cur_co;
        if self.cos.len() > 1 {
            if self.cos[me].idle_seen != seen {
                self.cos[me].idle_seen = seen;
                self.mt.count_idle(seen);
            }
            if self.mt.idle_all() {
                return Some(3);
            }
            return None;
        }
        self.mt.count_idle(seen);
        Some(3)
    }
}
