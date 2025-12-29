import { useRef, useEffect, useCallback } from 'react';
import uPlot from 'uplot';
import 'uplot/dist/uPlot.min.css';
import './LineChart.css';

// Channel colors (16 max)
const COLORS = [
  '#3b82f6',
  '#ef4444',
  '#22c55e',
  '#f59e0b',
  '#8b5cf6',
  '#ec4899',
  '#06b6d4',
  '#84cc16',
  '#f97316',
  '#6366f1',
  '#14b8a6',
  '#eab308',
  '#a855f7',
  '#f43f5e',
  '#0ea5e9',
  '#10b981',
];

interface LineChartProps {
  // Data: { channelName: [[timestamps], [values]] }
  data: Record<string, [number, number][]>;
  // Time range in seconds (for display width)
  timeRange?: number;
  // Whether the chart is paused
  isPaused?: boolean;
}

export default function LineChart({ data, isPaused = false }: LineChartProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<uPlot | null>(null);
  const channelsRef = useRef<string[]>([]);
  const isZoomedRef = useRef(false); // Track if user has drag-zoomed (frozen view)
  const manualYScaleRef = useRef<{ min: number; max: number } | null>(null); // Track wheel zoom Y scale
  const manualXRangeRef = useRef<number | null>(null); // Track wheel zoom X range (width in seconds)

  // Build uPlot data format: [timestamps, ...values]
  const buildChartData = useCallback(() => {
    // Sort channels alphabetically to maintain stable color order
    const channels = Object.keys(data).sort();
    if (channels.length === 0) {
      return { data: [[]] as uPlot.AlignedData, channels: [] };
    }

    // Collect all unique timestamps and sort
    const timestampSet = new Set<number>();
    for (const channel of channels) {
      const points = data[channel];
      for (const [t] of points) {
        timestampSet.add(t);
      }
    }
    const timestamps = Array.from(timestampSet).sort((a, b) => a - b);

    if (timestamps.length === 0) {
      return { data: [[]] as uPlot.AlignedData, channels: [] };
    }

    // Build value arrays for each channel
    const chartData: (number | null)[][] = [timestamps.map((t) => t / 1000)]; // Convert ms to seconds

    for (const channel of channels) {
      const points = data[channel];
      const valueMap = new Map<number, number>();
      for (const [t, v] of points) {
        valueMap.set(t, v);
      }

      const values = timestamps.map((t) => valueMap.get(t) ?? null);
      chartData.push(values);
    }

    return { data: chartData as uPlot.AlignedData, channels };
  }, [data]);

  // Create or update chart
  useEffect(() => {
    if (!containerRef.current) return;

    const { data: chartData, channels } = buildChartData();

    // If no data or no channels, destroy chart
    if (channels.length === 0 || chartData[0].length === 0) {
      if (chartRef.current) {
        chartRef.current.destroy();
        chartRef.current = null;
      }
      return;
    }

    // Check if channels changed
    const channelsChanged =
      channels.length !== channelsRef.current.length ||
      channels.some((ch, i) => ch !== channelsRef.current[i]);

    if (channelsChanged || !chartRef.current) {
      // Need to recreate chart
      if (chartRef.current) {
        chartRef.current.destroy();
      }

      const series: uPlot.Series[] = [
        { label: 'Time' }, // X axis
        ...channels.map((ch, i) => ({
          label: ch,
          stroke: COLORS[i % COLORS.length],
          width: 2,
          spanGaps: true,
        })),
      ];

      const opts: uPlot.Options = {
        width: containerRef.current.clientWidth,
        height: containerRef.current.clientHeight,
        series,
        scales: {
          x: {
            time: false, // We're using seconds, not Unix timestamps
            auto: false, // Manual scale control for zoom persistence
          },
          y: {
            auto: false, // Manual scale control for zoom persistence
          },
        },
        axes: [
          {
            stroke: '#888',
            grid: { stroke: '#333' },
            ticks: { stroke: '#444' },
          },
          {
            stroke: '#888',
            grid: { stroke: '#333' },
            ticks: { stroke: '#444' },
          },
        ],
        legend: {
          show: true,
        },
        cursor: {
          show: true,
          points: { show: true },
          drag: {
            x: true, // Enable drag to select X range
            y: true, // Enable drag to select Y range
            setScale: true, // Apply zoom on drag release
          },
        },
        hooks: {
          setSelect: [
            () => {
              isZoomedRef.current = true; // Mark as zoomed when user selects range
            },
          ],
          init: [
            (u) => {
              u.over.addEventListener('dblclick', () => {
                // Reset zoom flag and manual scales
                isZoomedRef.current = false;
                manualYScaleRef.current = null;
                manualXRangeRef.current = null;
                const data = u.data;
                if (data && data[0] && data[0].length > 0) {
                  const xData = data[0] as number[];
                  const xMin = Math.min(...xData);
                  const xMax = Math.max(...xData);
                  u.setScale('x', { min: xMin, max: xMax });

                  // Find Y range from all series
                  let yMin = Infinity;
                  let yMax = -Infinity;
                  for (let i = 1; i < data.length; i++) {
                    const yData = data[i] as (number | null)[];
                    for (const v of yData) {
                      if (v !== null) {
                        yMin = Math.min(yMin, v);
                        yMax = Math.max(yMax, v);
                      }
                    }
                  }
                  if (yMin !== Infinity && yMax !== -Infinity) {
                    const padding = (yMax - yMin) * 0.1;
                    u.setScale('y', { min: yMin - padding, max: yMax + padding });
                  }
                }
              });

              // Add wheel zoom
              u.over.addEventListener('wheel', (e) => {
                e.preventDefault();

                const factor = e.deltaY > 0 ? 1.1 : 0.9;
                const rect = u.over.getBoundingClientRect();
                const xPos = e.clientX - rect.left;
                const yPos = e.clientY - rect.top;

                // Get current scales
                const xScale = u.scales.x;
                const yScale = u.scales.y;

                // Check if scales are properly initialized (handle 0 as valid value)
                if (
                  xScale.min === undefined ||
                  xScale.max === undefined ||
                  yScale.min === undefined ||
                  yScale.max === undefined
                )
                  return;

                // Calculate cursor position in data space
                const xVal = u.posToVal(xPos, 'x');
                const yVal = u.posToVal(yPos, 'y');

                if (e.shiftKey) {
                  // Zoom X axis
                  const xRange = xScale.max - xScale.min;
                  const newRange = xRange * factor;
                  const ratio = (xVal - xScale.min) / xRange;
                  const newMin = xVal - newRange * ratio;
                  const newMax = xVal + newRange * (1 - ratio);
                  u.setScale('x', { min: newMin, max: newMax });
                  // Save X range width for sliding window behavior
                  manualXRangeRef.current = newRange;
                } else {
                  // Zoom Y axis
                  const yRange = yScale.max - yScale.min;
                  const newRange = yRange * factor;
                  const ratio = (yVal - yScale.min) / yRange;
                  const newMin = yVal - newRange * ratio;
                  const newMax = yVal + newRange * (1 - ratio);
                  u.setScale('y', { min: newMin, max: newMax });
                  // Save manual Y scale for data updates (but keep showing new data)
                  manualYScaleRef.current = { min: newMin, max: newMax };
                }
              });
            },
          ],
        },
      };

      chartRef.current = new uPlot(opts, chartData, containerRef.current);
      channelsRef.current = channels;
    } else {
      // Just update data, preserving zoom if user has zoomed
      const chart = chartRef.current;
      const prevXScale = { min: chart.scales.x.min, max: chart.scales.x.max };
      const prevYScale = { min: chart.scales.y.min, max: chart.scales.y.max };

      chart.setData(chartData);

      if (isZoomedRef.current) {
        // Restore previous zoom level
        if (prevXScale.min !== undefined && prevXScale.max !== undefined) {
          chart.setScale('x', { min: prevXScale.min, max: prevXScale.max });
        }
        if (prevYScale.min !== undefined && prevYScale.max !== undefined) {
          chart.setScale('y', { min: prevYScale.min, max: prevYScale.max });
        }
      } else {
        // Auto-scale to new data range
        if (chartData[0] && chartData[0].length > 0) {
          const xData = chartData[0] as number[];
          const xMin = Math.min(...xData);
          const xMax = Math.max(...xData);

          // Use manual X range if set (sliding window follows new data)
          if (manualXRangeRef.current !== null) {
            const range = manualXRangeRef.current;
            // Slide window to latest data
            chart.setScale('x', { min: xMax - range, max: xMax });
          } else {
            chart.setScale('x', { min: xMin, max: xMax });
          }

          let yMin = Infinity;
          let yMax = -Infinity;
          for (let i = 1; i < chartData.length; i++) {
            const yData = chartData[i] as (number | null)[];
            for (const v of yData) {
              if (v !== null) {
                yMin = Math.min(yMin, v);
                yMax = Math.max(yMax, v);
              }
            }
          }
          if (yMin !== Infinity && yMax !== -Infinity) {
            // Use manual Y scale if set by wheel zoom, otherwise auto-scale
            if (manualYScaleRef.current) {
              chart.setScale('y', manualYScaleRef.current);
            } else {
              const padding = (yMax - yMin) * 0.1 || 1;
              chart.setScale('y', { min: yMin - padding, max: yMax + padding });
            }
          }
        }
      }
    }

    return () => {
      // Cleanup on unmount
    };
  }, [buildChartData]);

  // Handle resize
  useEffect(() => {
    if (!containerRef.current) return;

    const resizeObserver = new ResizeObserver((entries) => {
      if (chartRef.current && entries[0]) {
        const { width, height } = entries[0].contentRect;
        chartRef.current.setSize({ width, height });
      }
    });

    resizeObserver.observe(containerRef.current);

    return () => {
      resizeObserver.disconnect();
    };
  }, []);

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      if (chartRef.current) {
        chartRef.current.destroy();
        chartRef.current = null;
      }
    };
  }, []);

  return <div ref={containerRef} className={`line-chart-container ${isPaused ? 'paused' : ''}`} />;
}
