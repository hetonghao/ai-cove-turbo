# Turbo 真实使用分布校准

这层校准复用现有四路公网 benchmark，不连接生产数据库，也不会自动修改 `src/telemetry.js`。生产侧只负责导出匿名聚合 profile；Turbo benchmark 负责把历史分布与本次传输机制样本合并成候选常量报告。

## 运行边界

- 公网模型 benchmark 会产生付费请求，只有获得明确授权并注入 `AI_COVE_API_KEY` 后才能运行。
- profile 不得包含用户 ID、提示词、IP、Token 名称、请求 ID、API Key 或完整请求头；解析器拒绝未声明字段。
- `measurement_date` 的 `anchor_date` 必须是 D-15，且历史至少三天。少于五天时，每个缺失日期必须使用固定原因码说明。
- 每条核心路径必须有至少 12 个无重试样本；Hybrid 每个有效样本必须包含首轮 HTTP 和至少两个 warm WS continuation，且期间不得发生重连。
- 任一日期的同窗或全天匹配覆盖率低于 70%，或历史方向与机制层相反时，不生成候选常量。

## Profile 口径

可复制的完整脱敏示例见 [2026-08-10-workload-profile.example.json](./2026-08-10-workload-profile.example.json)。

`filters` 固定模型、渠道、`/v1/responses` 和流式状态。`bucket` 的四个键必须全部相同才算匹配：

- `input`：由当天 Turbo 输入 token 分位数冻结出的桶序号。
- `output`：边界固定为 `100 / 300 / 1000 / 3000`，对应五档输出。
- `cached_ratio`：由 profile 声明的缓存占比边界产生的桶序号。
- `reasoning_effort`：精确值；未知值必须单列，不能与其他档合并。

`current` 还需提供 HTTP/WS 请求数、raw/sent bytes 的 P50/P90，以及首事件和 complete 的 P50/P90。每个历史日期同时提供 `same_window` 与 `full_day`。历史桶少于 12 条或缺少 P90 时，报告保留 P50、样本数和覆盖率，但不构造 P90。

多桶统计先按当天分布标准化每个历史日，再对日期取中位数，因此每个日期等权，不按历史请求量加权。历史 before/after 只用于真实体验交叉验证；WebSocket 固定节省毫秒只来自 Turbo HTTP 与 Hybrid 纯 WS warm rounds 的差值。上传压缩收益单独按 5、10、20 Mbps 输出，避免重复计入 WS 固定收益。

候选报告在每个历史日的同窗和全天范围下逐桶输出当前样本数、历史样本数和 `profileCoveragePct`；该值是该匹配桶对当前 profile 总覆盖率的贡献，同一范围内求和等于逐日总覆盖率。

## 执行

```bash
cd /Users/hetonghao/dev/hth-project/ai-cove/turbo

export AI_COVE_API_KEY
export TURBO_BENCHMARK_WORKLOAD_PROFILE=/absolute/path/to/anonymized-workload-profile.json
export TURBO_BENCHMARK_CANDIDATE_OUTPUT=/absolute/path/to/candidate-constants.json
export TURBO_BENCHMARK_RUNS=12
export TURBO_BENCHMARK_WARMUPS=1
export TURBO_BENCHMARK_MODEL=gpt-5.6-luna

npm run benchmark:live
```

终端输出包含四路 `median[min,max]`、逐样本传输分类、逐日历史校准和旧值/候选值变化。`TURBO_BENCHMARK_CANDIDATE_OUTPUT` 是机器可读 JSON；状态固定为 `candidate_not_applied`，需要人工审阅后另行决定是否更新产品常量。
