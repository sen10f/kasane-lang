# cache-integrity
H3-01/H3-02 の対応: キャッシュ置き場の 0700・持ち主検査、エントリと生成物の SHA-256 マニフェスト、共有 ksn の自己ハッシュ照合。実装は `stage0/src/cli.rs` の `safe_cache_dir` / `entry_intact` / `artifact_intact`。
