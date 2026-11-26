# tools/fix-rust-forms-more

`ksn fix` に E0052 (裸の Vec/String/HashMap/BTreeMap/VecDeque/Box → 完全な道筋) と 1 行の閉包本体 (E0056) を追加。
未着手 (直し方が 1 つに決まる候補): `Ok(mut s)` → `Ok(s0)` + `let mut s = s0` (E0002)。曖昧で診断のまま: `|x|` 閉包 (型が要る)・`<<`・シャドーイング E1021。
計測: bench の Nemotron 初稿 46 本を fix 後に check: 15 → 16 本が診断 0。残りは型・借用の意味の誤り (E2020/E1108/E2044)。
