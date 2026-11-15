//! `std.task` の協調実行器 (仕様 5.6、第 1 段)。木走査 (`ksn run` の既定、`ksn test`) で動く。
//!
//! 中断 (`uses [suspend]` の関数の呼び出し、ここでは `yield_now` と `Task.join_task`) は、木走査の Rust のスタックを
//! 途中で止めて、後で同じ場所から続けることで実現する。そのためタスクごとに OS スレッドを 1 本作り、
//! 「いま走ってよいタスクは 1 つ」という合図 (`Shared.cur`) を Mutex と Condvar で渡す。
//!
//! - 合図を持つスレッドだけがインタプリタ (`Interp`、値の `Rc`) に触る。合図を渡すスレッドは、渡したあと
//!   インタプリタに触らない。したがって `Interp` へのアクセスは同時に 1 つだけで、Mutex が順序を保証する。
//! - 切り替えの順は決定的: 中断したタスク `me` の次の id から順に見て、走れる (`Runnable`) タスク、または待っていた
//!   相手が終わった (`WaitingFor(t)` で `t` が `Done`) タスクを選ぶ。同じプログラムは何度動かしても同じ順になる。
//! - `spawn_local` は走らせない。タスクは、`block_on` の本体・他のタスクが `yield_now` / `join` で譲ったとき
//!   に走り出す。`block_on` は本体が終わったあと、まだ終わっていないタスクを全部 `join` してから返る (構造化)。
//! - タスクのどれかが panic (または `exit`) すると、そのエラーは根のタスク (`block_on` を呼んだ側) へ渡り、
//!   根が `Err` で戻る。panic はプロセスを中断する (5.2) ので、残りのタスクは捨てられる。
//! - 待つ相手がなく、どのタスクも進めない (join の循環、自分自身の join など) ときは、`deadlock` の panic。
//!
//! 第 1 段の制限: タスクは id の順に走るだけで、優先度はない。タスクの取り消しはない。`block_on` の入れ子は不可。
//! 生成したタスクは、`block_on` の中の `join` で受けない限りそのまま終わるまで走る (線形でないハンドル)。

use std::sync::{Arc, Condvar, Mutex};

use crate::interp::Interp;
use crate::value::*;

/// 全タスクが互いに待ち合って誰も進めないときの panic の文面。
pub const DEADLOCK_TASK: &str = "deadlock: every std.task task is waiting for another task that cannot finish (a join cycle, or a join on the task itself)";

/// 取り消されたタスクの結果を `join_task` で受けたときの panic の文面。木走査とネイティブで同じ文面にする。
pub const TASK_CANCELLED: &str = "std.task.Task.join_task: the task was cancelled";

/// タスクのスレッドのスタック量 (木走査の再帰の深さの上限 (約 2500 フレーム) を支える。`main` と同じ)。
/// 仮想メモリの予約なので、使わない分は実メモリを取らない。
const STACK_BYTES: usize = 1 << 30;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum St {
    /// 走れる (中断したあと、あるいは生成直後で、待つ条件がない)。
    Runnable,
    /// 相手のタスク (id) が終わるのを待っている。
    WaitingFor(usize),
    /// 終わった (結果またはエラーを持つ)。
    Done,
}

/// スレッドどうしで共有する部分。ここには `Val` (`Rc`) を置かない。
struct Shared {
    /// いま合図を持つタスクの id。
    cur: usize,
    /// タスクの状態 (id が添字。0 は根のタスク)。
    states: Vec<St>,
}

struct Sched {
    m: Mutex<Shared>,
    cv: Condvar,
}

impl Sched {
    /// 合図が `me` に来るまで待つ。
    fn wait_turn(&self, me: usize) {
        let mut g = self.m.lock().unwrap();
        while g.cur != me {
            g = self.cv.wait(g).unwrap();
        }
    }
}

/// タスク 1 つぶんのインタプリタ側の値 (合図を持つスレッドだけが触る)。
struct Slot {
    /// 未開始のタスクの本体 (クロージャ)。開始時に取り出す。
    body: Option<Val>,
    /// 完了したタスクの結果。
    result: Option<Val>,
    /// 取り消された (仕様 5.6 の第 4 段)。取り消したタスクは、この欄を立てて合図を渡し、本体を抜けて終わる。
    cancelled: bool,
}

/// `Interp` が持つ、実行器の状態。`block_on` の間だけ `sched` が `Some`。
#[derive(Default)]
pub struct Tasks {
    sched: Option<Arc<Sched>>,
    slots: Vec<Slot>,
    /// いま走っているタスクの id (合図を受け取ったあとに書く)。
    cur: usize,
    /// 根以外のタスクが投げたエラー。根のタスクへ渡す (根が次に合図を受けたとき `Err` で返る)。
    error: Option<Ctl>,
    /// 取り消したタスクの番号。取り消されたタスクは、終わるときに合図をこの番号へ返す。
    cancel_by: usize,
}

/// タスクのスレッドへ `Interp` の生ポインタを渡すための包み。合図の順序で一度に 1 つだけが使う。
struct SendPtr(*mut Interp);
// SAFETY: 生ポインタの指す `Interp` へは、合図を持つスレッドだけがアクセスする (モジュールの説明)。
// 合図の受け渡しは Mutex で行うので、アクセスどうしは happens-before で順序づけられる。
unsafe impl Send for SendPtr {}

impl SendPtr {
    /// 包みを丸ごと取り出す (クロージャが生ポインタの欄だけを捕まえないようにする)。
    fn get(self) -> *mut Interp {
        self.0
    }
}

/// 候補を `me` の次から見て、走れるタスクを 1 つ選ぶ。`WaitingFor` で相手が終わっていれば走れる状態に戻す。
fn pick(states: &mut [St], me: usize) -> Option<usize> {
    let n = states.len();
    for k in 1..=n {
        let i = (me + k) % n;
        match states[i] {
            St::Runnable => return Some(i),
            St::WaitingFor(t) if states[t] == St::Done => {
                states[i] = St::Runnable;
                return Some(i);
            }
            _ => {}
        }
    }
    None
}

impl Interp {
    /// 実行器の中か (`block_on` の本体の間)。
    pub fn task_running(&self) -> bool {
        self.tasks.sched.is_some()
    }

    fn task_state(&self, i: usize) -> St {
        let sched = self.tasks.sched.as_ref().expect("std.task: no executor");
        let g = sched.m.lock().unwrap();
        g.states[i]
    }

    fn task_set_state(&self, i: usize, s: St) {
        let sched = self.tasks.sched.as_ref().expect("std.task: no executor");
        sched.m.lock().unwrap().states[i] = s;
    }

    /// `std.task.block_on`: 根のタスクとして `f` を走らせ、残りのタスクを全部終わらせてから結果を返す。
    pub fn task_block_on(&mut self, f: Val) -> Res {
        if self.task_running() {
            return panic("std.task.block_on: already running (nested block_on is not supported in stage 1)");
        }
        let sched = Arc::new(Sched { m: Mutex::new(Shared { cur: 0, states: vec![St::Runnable] }), cv: Condvar::new() });
        self.tasks = Tasks { sched: Some(sched), slots: vec![Slot { body: None, result: None, cancelled: false }], cur: 0, error: None, cancel_by: 0 };
        let r = match self.call_value(&f, vec![]) {
            Ok(v) => self.task_drain().map(|_| v),
            Err(e) => Err(e),
        };
        // 実行器を畳む。まだ終わっていないタスク (エラーで中断したとき) のスレッドは、合図を待ったまま残る
        // (プロセスの終了で消える)。
        self.tasks = Tasks::default();
        r
    }

    /// 根のタスクの本体が終わったあと、まだ終わっていないタスクを (id の順に) 全部待つ。
    fn task_drain(&mut self) -> Result<(), Ctl> {
        loop {
            let n = self.tasks.slots.len();
            let next = (1..n).find(|&i| self.task_state(i) != St::Done);
            let Some(i) = next else { return Ok(()) };
            self.task_wait_for(i)?;
        }
    }

    /// `std.task.spawn_local`: タスクを作る (まだ走らせない)。返すのはタスクの id。
    pub fn task_spawn(&mut self, f: Val) -> Result<usize, Ctl> {
        let Some(sched) = self.tasks.sched.clone() else {
            return panic("std.task.spawn_local: no executor (call it inside std.task.block_on)");
        };
        let id = self.tasks.slots.len();
        let ptr = SendPtr(self as *mut Interp);
        let s2 = sched.clone();
        // スレッドを先に作る (失敗したらタスクを登録しない)。新しいスレッドは合図が来るまで何もしない。
        std::thread::Builder::new()
            .stack_size(STACK_BYTES)
            .spawn(move || {
                let p = ptr;
                s2.wait_turn(id);
                // SAFETY: 合図を受けたので、いまこのスレッドだけが `Interp` に触る (`SendPtr` の説明)。
                let it: &mut Interp = unsafe { &mut *p.get() };
                it.depth = 0;
                it.tasks.cur = id;
                let body = it.tasks.slots[id].body.take();
                let r = match body {
                    // 始まる前に取り消されたときは、本体を走らせない (枠は作られているが、中身の値は持たない)。
                    Some(b) if !it.tasks.slots[id].cancelled => it.call_value(&b, vec![]),
                    _ => Ok(Val::Unit),
                };
                match r {
                    Ok(v) => it.tasks.slots[id].result = Some(v),
                    // 取り消しの巻き戻し: エラーではない。結果は持たない。
                    Err(Ctl::Cancel) => {}
                    Err(e) => it.tasks.error = Some(e),
                }
                // 取り消されたタスクは、終わったあと合図を取り消したタスクへ返す (中断点で止まる)。
                if it.tasks.slots[id].cancelled {
                    let mut g = s2.m.lock().unwrap();
                    g.states[id] = St::Done;
                    g.cur = it.tasks.cancel_by;
                    drop(g);
                    s2.cv.notify_all();
                    return;
                }
                // ここから先は、合図を渡す (インタプリタに触らない)。渡す前に、次の走者へ渡すエラーを確定させる。
                let had_err = it.tasks.error.is_some();
                let mut g = s2.m.lock().unwrap();
                g.states[id] = St::Done;
                let next = if had_err {
                    g.states[0] = St::Runnable;
                    Some(0)
                } else {
                    pick(&mut g.states, id)
                };
                match next {
                    Some(n) => g.cur = n,
                    None => {
                        // 誰も進めない: 根へ deadlock を渡す。
                        it.tasks.error = Some(Ctl::Panic(DEADLOCK_TASK.to_string()));
                        g.states[0] = St::Runnable;
                        g.cur = 0;
                    }
                }
                drop(g);
                s2.cv.notify_all();
            })
            .map_err(|e| Ctl::Panic(format!("std.task.spawn_local: cannot create a task thread ({})", e)))?;
        // 合図はまだ根のタスクが持つ。スレッドは合図が `id` に来るまで `Interp` に触らないので、ここで登録してよい。
        self.tasks.slots.push(Slot { body: Some(f), result: None, cancelled: false });
        sched.m.lock().unwrap().states.push(St::Runnable);
        Ok(id)
    }

    /// `std.task.yield_now`: 走れる他のタスクがあれば、そちらへ譲る。なければそのまま戻る。
    /// 実行器の外では何もしない。
    pub fn task_yield(&mut self) -> Result<(), Ctl> {
        if !self.task_running() {
            return Ok(());
        }
        let me = self.tasks.cur;
        self.task_switch(me)
    }

    /// `Task.join_task`: タスク `id` が終わるまで待ち、その結果を返す。取り消されたタスクは結果を持たないので panic。
    pub fn task_join(&mut self, id: usize) -> Res {
        if !self.task_running() {
            return panic("std.task.Task.join_task: no executor (the task was not created inside std.task.block_on)");
        }
        if id >= self.tasks.slots.len() {
            return panic("std.task.Task.join_task: unknown task");
        }
        self.task_wait_for(id)?;
        if self.tasks.slots[id].cancelled {
            return panic(TASK_CANCELLED);
        }
        Ok(self.tasks.slots[id].result.clone().unwrap_or(Val::Unit))
    }

    /// `Task<T>.cancel` (仕様 5.6 の第 4 段): タスク `id` を取り消す。終わったタスクへの取り消しは何もしない。
    /// 取り消されたタスクは、次に合図を受けたところ (中断点) で本体を抜けて終わり、結果を持たない。
    /// 合図は取り消したタスクから取り消されたタスクへ渡り、その後、取り消したタスクへ戻る (同期の巻き戻し)。
    /// 中断点をまたぐ値は、取り消しの対象になる前の検査 (E4022) で、解放の要る値を持たないと確かめてある。
    pub fn task_cancel(&mut self, id: usize) -> Result<(), Ctl> {
        if !self.task_running() {
            return panic("std.task.Task.cancel: no executor (the task was not created inside std.task.block_on)");
        }
        let me = self.tasks.cur;
        if id >= self.tasks.slots.len() {
            return panic("std.task.Task.cancel: unknown task");
        }
        if id == me {
            return panic("std.task.Task.cancel: a task cannot cancel itself");
        }
        if self.task_state(id) == St::Done {
            return Ok(());
        }
        self.tasks.slots[id].cancelled = true;
        self.tasks.cancel_by = me;
        let sched = self.tasks.sched.clone().expect("std.task: no executor");
        let depth = self.depth;
        // 待っている間に別のスレッドが `Interp` を書き換えるので、`task_switch` と同じく生ポインタで読み書きする。
        let p: *mut Interp = std::hint::black_box(self as *mut Interp);
        sched.m.lock().unwrap().cur = id;
        sched.cv.notify_all();
        sched.wait_turn(me);
        // SAFETY: 合図が戻った時点で、このスレッドだけが `Interp` に触る (モジュールの説明)。
        let it = unsafe { &mut *p };
        it.depth = depth;
        it.tasks.cur = me;
        Ok(())
    }

    /// 相手 `target` が終わるまで、いまのタスクを待たせる (ほかのタスクを走らせる)。
    fn task_wait_for(&mut self, target: usize) -> Result<(), Ctl> {
        let me = self.tasks.cur;
        while self.task_state(target) != St::Done {
            self.task_set_state(me, St::WaitingFor(target));
            self.task_switch(me)?;
        }
        self.task_set_state(me, St::Runnable);
        Ok(())
    }

    /// タスク `me` が走るのをやめ、次の走者へ合図を渡して、自分の合図が戻るまで待つ。
    /// 次の走者がいなければ deadlock。戻ったとき、根に届いたエラーがあれば `Err` で返す。
    fn task_switch(&mut self, me: usize) -> Result<(), Ctl> {
        let sched = self.tasks.sched.clone().expect("std.task: no executor");
        let next = {
            let mut g = sched.m.lock().unwrap();
            pick(&mut g.states, me)
        };
        let Some(next) = next else {
            return panic(DEADLOCK_TASK);
        };
        if next == me {
            return Ok(());
        }
        // 合図を渡す前に、このスレッドのインタプリタの状態 (木走査の深さ) を控える。
        let depth = self.depth;
        // 待っている間に別のスレッドが `Interp` を書き換える。`self` はこの関数の `&mut` のままでは、
        // 最適化器が「待つ間は変わらない」と見て、戻したあとの書き込みを省くことがある (実際に深さが狂った)。
        // 生ポインタに逃がし、待ちの前後の読み書きはすべてそれを通して行う (待ちは不透明な呼び出し)。
        let p: *mut Interp = std::hint::black_box(self as *mut Interp);
        sched.m.lock().unwrap().cur = next;
        sched.cv.notify_all();
        sched.wait_turn(me);
        // SAFETY: 合図が戻った時点で、このスレッドだけが `Interp` に触る (モジュールの説明)。
        // `p` は上で `self` から作り、待ちをまたいで使う。
        let it = unsafe { &mut *p };
        // 合図が戻った。このタスクの状態を戻す。
        it.depth = depth;
        it.tasks.cur = me;
        // 取り消されたタスクは、ここで止まる (中断点から戻らず、本体を抜ける)。
        if it.tasks.slots[me].cancelled {
            return Err(Ctl::Cancel);
        }
        if me == 0 {
            if let Some(e) = it.tasks.error.take() {
                return Err(e);
            }
        }
        Ok(())
    }
}
