//! ホットリロード (設計_KIR.md 4.4 / 7 章 9)。`ksn run --watch --engine=vm`。
//!
//! 監視スレッド (`cli::spawn_watcher_with`) は、外部面 (ヘッダ・型) が変わらない関数本体の変更を検出すると、再構築した `Program` を送って
//! エポック (`flag`) を立てる。ヘッダや型の変更は、木走査と同じに「再起動が要る」と stderr に出すだけで反映しない。
//!
//! VM は関数表の版 (`Vm::vers`) を持つ。安全点 (関数の入口 = `Call`) でエポックを見て、立っていれば新しい `Program` を KIR に下ろして
//! バイトコードにし、新しい版として加える。以降の呼び出しは、呼び元がどの版でも **最新の版の関数** (鍵で引く) に解決する。
//! すでに走っているフレームは、始まったときの版のコードを最後まで走らせる (木走査が古い構文木で走り続けるのと同じ)。
//! エポックの読みは、`--watch` でないときは `hook` が `None` なので、呼び出しごとの追加の費用はない。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;

use super::bc::Prog;
use super::run::Vm;
use crate::kir_lower;
use crate::program::Program;

/// 監視スレッドとの接続。
pub struct Reload {
    /// 再構築した `Program` と利用者の定義の添字。
    pub rx: Receiver<(Program, Vec<usize>)>,
    /// 新しい版が送られたら立つ (VM が降ろす)。
    pub flag: &'static AtomicBool,
    pub entry: String,
}

impl<'a> Vm<'a> {
    /// 呼び先 `ci` (版 `vi` の関数番号) を最新の版の関数へ解決する。安全点でもある。
    pub(super) fn resolve_call(&mut self, vi: u32, ci: u32) -> (u32, u32) {
        if self.hook.as_ref().is_some_and(|h| h.flag.load(Ordering::Acquire)) {
            self.apply_reload();
        }
        let cur = (self.vers.len() - 1) as u32;
        if cur == vi {
            return (vi, ci);
        }
        // SAFETY: `vers` の指す `Prog` は `owned` (または `Vm::prog`) が `Vm` の寿命の間保持する。
        let old: &Prog = unsafe { &*self.vers[vi as usize] };
        let new: &Prog = unsafe { &*self.vers[cur as usize] };
        match new.by_key.get(&old.funcs[ci as usize].key) {
            Some(n) => (cur, *n),
            None => (vi, ci),
        }
    }

    /// 送られてきた最新の `Program` を新しい版にする。下ろせない・検査に通らないときは古いコードのまま続ける。
    fn apply_reload(&mut self) {
        let Some(h) = &self.hook else { return };
        h.flag.store(false, Ordering::Release);
        let mut last = None;
        while let Ok(x) = h.rx.try_recv() {
            last = Some(x);
        }
        let Some((p, user)) = last else { return };
        let entry = h.entry.clone();
        let lo = kir_lower::lower_program(&p, &user, Some(&entry));
        let r = super::prepare(lo, &entry);
        self.flush();
        match r {
            Ok(bc) => {
                let b = Box::new(bc);
                self.vers.push(&*b as *const Prog);
                self.owned.push(b);
                eprintln!("hot reload: function bodies updated");
            }
            Err(why) => eprintln!("hot reload: {} (kept old code)", why.lines().next().unwrap_or("")),
        }
    }
}
