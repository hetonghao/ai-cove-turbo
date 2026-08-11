(() => {
  "use strict";

  const numberFormatter = new Intl.NumberFormat("zh-CN");
  // 2026-08-09 公网 12 轮基准：5 轮直连中位数与 Turbo HTTP 到 Hybrid 的中位数差。
  const SPEED_ESTIMATE = Object.freeze({
    uplinkBitsPerSecond: 10_000_000,
    baselineFirstTokenMs: 1_661,
    baselineCompleteMs: 2_273,
    websocketFirstTokenSavedMs: 469,
    websocketCompleteSavedMs: 274,
  });

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
      month: "2-digit",
      day: "2-digit",
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

  function estimateSpeed(buckets, transport, result) {
    const totals = (buckets ?? []).flatMap((bucket) => bucket?.series ?? [])
      .filter((series) => (transport === "all" || series.transport === transport)
        && (result === "all" || series.result === result)
        && series.result !== "error")
      .reduce((estimate, series) => {
        const requests = Number(series.requests) || 0;
        return {
          requests: estimate.requests + requests,
          savedBytes: estimate.savedBytes + Math.max(0, (Number(series.rawBytes) || 0) - (Number(series.sentBytes) || 0)),
          websocketRequests: estimate.websocketRequests + (series.transport === "WS" ? requests : 0),
        };
      }, { requests: 0, savedBytes: 0, websocketRequests: 0 });
    if (!totals.requests) return { requests: 0, firstPercent: 0, completePercent: 0 };

    const uploadSavedMs = totals.savedBytes * 8 * 1_000 / SPEED_ESTIMATE.uplinkBitsPerSecond;
    const firstSavedMs = uploadSavedMs + totals.websocketRequests * SPEED_ESTIMATE.websocketFirstTokenSavedMs;
    const completeSavedMs = uploadSavedMs + totals.websocketRequests * SPEED_ESTIMATE.websocketCompleteSavedMs;
    return {
      requests: totals.requests,
      firstPercent: Math.min(99, firstSavedMs / (totals.requests * SPEED_ESTIMATE.baselineFirstTokenMs) * 100),
      completePercent: Math.min(99, completeSavedMs / (totals.requests * SPEED_ESTIMATE.baselineCompleteMs) * 100),
    };
  }

  function formatSpeedGain(estimate) {
    if (!estimate?.requests) return "— / —";
    return `${estimate.firstPercent.toFixed(1)}% / ${estimate.completePercent.toFixed(1)}%`;
  }

  function selectWindow(windows, minutes) {
    return (windows ?? []).find((window) => Number(window.minutes) === Number(minutes)) ?? null;
  }

  window.TurboTelemetry = Object.freeze({
    bucketTotals,
    estimateSpeed,
    formatBytes,
    formatChartTime,
    formatClock,
    formatRate,
    formatSpeedGain,
    granularityLabel,
    selectWindow,
    summarizeBuckets,
  });
})();
