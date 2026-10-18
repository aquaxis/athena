# SHOULD 要件の適合記録(X-06)

| 項目 | 内容 |
|---|---|
| 方法 | 要件文書の SHOULD レベル要件をすべて監査する。遵守済み、または根拠付きの逸脱として記録(`X-06` の規則 — 無言の逸脱は禁止) |
| 状態 | Phase 8 完了時点で監査(2026-10-18) |
| 英語版 | `docs/should_compliance.md`(同一内容) |

## 1. 遵守

| 要件 | 遵守の根拠 |
|---|---|
| `ATH-CORE-015` — `unknowns` が空でないときの調査 PlanStep | Agent 側の指導として遵守。producer のプロトコルファイルが規則を運ぶ(`agents/producer/core-requests.md`)。Core Host は強制できない(選択は Agent のもの)— guidance-held として記録 |
| `ATH-AGENT-006` — 複数候補のスコアリング | **実装済み**(P8-01、`athena-agent::scoring`、仕様 §8.4 の式) |
| `R-AGT-06` — 1 PlanStep に 1 Worker Instance | 構造により遵守: v1 の Instance は正確に 1 つの PlanStep を担う(`ATH-HIER-014` のスコーピング。文脈は Artifacts/Results と `depends_on` で引き継ぐ)。Adapter/委任コンテキストのテストが検証 |
| `R-CFG-09` — 再読み込みは再起動による。ホット変更は skill 有効化のみ | 遵守: 設定のホットリロード経路は存在しない。Registry の `refresh` が唯一のホット操作(`ATH-SKILL-008`) |
| `R-TST-10` — ライブラリクレートの単体カバレッジ ≥ 80 % | `cargo llvm-cov --workspace` で計測: 合計 83.8 % 行 / 85.5 % 関数。ライブラリクレート合算(2 つの bin アプリケーションの boot 面を除く)は **89.3 %** — X-03 の記録を参照 |
| NFR-11 / NFR-12 — 起動 ≤ 3 秒、ログ < 1 MB/Task | 当時は未計測(常駐プロセスの行、X-01 の未完集合 — 実装との矛盾なし)。その後 2026-10-18 に実ランナーで計測され、NFR 適合記録に記載 |

## 2. 逸脱とその根拠

### D-SHOULD-1 — トークン失効(R-SEC-03 の「SHOULD トークン失効を設定」)

**逸脱の内容。** `athena-api::auth::TokenRegistry::issue` は失効時刻を記録しない。発行された
トークンは取り消されるまで静的である。

**根拠。** v1 の認証は要件上**静的発行の Bearer トークン**である(`R-SEC-04`): 単一ユーザー、
単一ホスト、loopback 既定。更新の手順を伴わない失効は、操作者のセッション途中をデッドロック
させる(Task の承認待ちは 1 時間のトークンより長く続く)。また、長い思考の間も残る承認/操作の
痕跡を監査行として結合可能に保つには**安定した principal** が必要である(`ATH-OBS-004`)。
ロールチェックの位置(`athena-api::roles`)は後の認証アップグレードで存続すべき部分であり
(`ATH-SEC-027`)、すでに隔離されている — 失効の追加は `authenticate` の拡張であって、
エンドポイント毎のチェックを決して変更しない。

**再有効化の条件。** OIDC/OAuth2 の作業とともに(`R-SEC-04` の後日の追加として):
`issue` が `expires_at` を持ち、`load` が boot 時に失効行を拒否し、`authenticate` が失効
トークンを 401 封筒と監査 Event で拒否する。認証を扱う仕様節を同一変更単位で改訂する。

### D-SHOULD-2 — 外部取得 Skill に対するより強い隔離(R-SEC-11)

**逸脱の内容。** 出荷する Runtime 層が実装する境界はプロセス境界のみ
(`IsolationBoundary::Process`)であり、v1 はコンテナ Runtime Adapter を同梱しない。

**根拠。** 外部取得の Skill は 1 つも取り込まれていない(`ATH-SKILL-036` の v1 既定は空の
allow-list によりすべての外部取得を拒否)。したがって SHOULD が防ぐ暴露は発生しない:
選択集合の Skill はすべてプロジェクト内で、チェックサムと読み取り専用の制御表面に拘束される。
境界の**宣言**の面は実装済みで、現在強制可能である: `RuntimeCapabilities.isolation` は
Runtime が実際に提供するものに対して検証され(`ATH-SEC-015`)、`at_least_as_strong_as` が
R-SEC-11 の考察する upgrade 比較を与える — コンテナ Adapter はより強い境界を registry に
登録でき、ハーネスのゲート(外部来歴を持つ Skill の経路選択)は Core 変更なしで必要な事実を
得る(`ATH-RT-007`)。

**再有効化の条件。** 最初の外部取得 Skill を受け入れる前に: Runtime Interface を実装し共有
契約スイートに通るコンテナ Runtime Adapter、`skills.allowed_publishers`/`allowed_sources`
の設定、および「外部 Skill を使う Agent は `isolation ≥ Container` のみ」の経路規則を選択面に
配線すること。
