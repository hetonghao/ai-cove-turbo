(() => {
  "use strict";

  const numberFormatter = new Intl.NumberFormat("zh-CN");

  function formatBytes(value) {
    const bytes = Number(value) || 0;
    if (bytes >= 1_000_000) return `${(bytes / 1_000_000).toFixed(2)} MB`;
    if (bytes >= 1_000) {
      const kilobytes = bytes / 1_000;
      return `${kilobytes.toFixed(Number.isInteger(kilobytes) ? 0 : 1)} KB`;
    }
    return `${numberFormatter.format(bytes)} B`;
  }

  function formatRate(rawValue, sentValue) {
    const raw = Number(rawValue) || 0;
    const sent = Number(sentValue) || 0;
    if (!raw) return "—";
    return `${Math.max(0, (1 - sent / raw) * 100).toFixed(1)}%`;
  }

  function formatClock(timestampMs) {
    return new Intl.DateTimeFormat("zh-CN", {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
      hourCycle: "h23",
    }).format(new Date(Number(timestampMs) || 0));
  }

  function formatChartTime(timestampMs, rangeMinutes) {
    const options = { hour: "2-digit", minute: "2-digit", hourCycle: "h23" };
    if (rangeMinutes === 1) options.second = "2-digit";
    if (rangeMinutes === 1_440) {
      options.month = "numeric";
      options.day = "numeric";
    }
    return new Intl.DateTimeFormat("zh-CN", options)
      .format(new Date(Number(timestampMs) || 0))
      .replace(/\s+/g, " ");
  }

  function granularityLabel(bucketSeconds) {
    const seconds = Number(bucketSeconds) || 0;
    if (seconds < 60) return `每 ${seconds} 秒`;
    if (seconds < 3_600) return `每 ${seconds / 60} 分钟`;
    return `每 ${seconds / 3_600} 小时`;
  }

  function bucketTotals(bucket, transport, result) {
    return (bucket?.series ?? [])
      .filter((series) => (transport === "all" || series.transport === transport)
        && (result === "all" || series.result === result))
      .reduce((totals, series) => ({
        requests: totals.requests + (Number(series.requests) || 0),
        rawBytes: totals.rawBytes + (Number(series.rawBytes) || 0),
        sentBytes: totals.sentBytes + (Number(series.sentBytes) || 0),
      }), { requests: 0, rawBytes: 0, sentBytes: 0 });
  }

  function summarizeBuckets(buckets, transport, result) {
    return (buckets ?? []).reduce((totals, bucket) => {
      const current = bucketTotals(bucket, transport, result);
      return {
        requests: totals.requests + current.requests,
        rawBytes: totals.rawBytes + current.rawBytes,
        sentBytes: totals.sentBytes + current.sentBytes,
      };
    }, { requests: 0, rawBytes: 0, sentBytes: 0 });
  }

  function selectWindow(windows, minutes) {
    return (windows ?? []).find((window) => Number(window.minutes) === Number(minutes)) ?? null;
  }

  window.TurboTelemetry = Object.freeze({
    bucketTotals,
    formatBytes,
    formatChartTime,
    formatClock,
    formatRate,
    granularityLabel,
    selectWindow,
    summarizeBuckets,
  });
})();
