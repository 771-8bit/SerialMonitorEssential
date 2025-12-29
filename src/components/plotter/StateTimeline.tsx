import { useRef, useEffect, useCallback } from 'react';
import './StateTimeline.css';

// Types for state data
export interface StateSegment {
    start_ms: number;
    end_ms: number;
    state: string;
}

export interface StateRow {
    channel: string;
    segments: StateSegment[];
}

interface StateTimelineProps {
    rows: StateRow[];
    timeMin: number; // in seconds
    timeMax: number; // in seconds
}

// Default colors for common states
const DEFAULT_STATE_COLORS: Record<string, string> = {
    ON: '#22c55e',
    OFF: '#6b7280',
    RUNNING: '#22c55e',
    STOPPED: '#ef4444',
    IDLE: '#f59e0b',
    ERROR: '#dc2626',
    WARNING: '#f59e0b',
    OK: '#22c55e',
    TRUE: '#22c55e',
    FALSE: '#6b7280',
    '1': '#22c55e',
    '0': '#6b7280',
};

// Color cache for session consistency
const colorCache = new Map<string, string>();

function hashColor(str: string): string {
    let hash = 0;
    for (let i = 0; i < str.length; i++) {
        hash = str.charCodeAt(i) + ((hash << 5) - hash);
        hash = hash & hash;
    }
    const hue = Math.abs(hash) % 360;
    return `hsl(${hue}, 65%, 50%)`;
}

function getStateColor(state: string): string {
    const upperState = state.toUpperCase();
    if (DEFAULT_STATE_COLORS[upperState]) {
        return DEFAULT_STATE_COLORS[upperState];
    }
    if (colorCache.has(state)) {
        return colorCache.get(state)!;
    }
    const color = hashColor(state);
    colorCache.set(state, color);
    return color;
}

export default function StateTimeline({ rows, timeMin, timeMax }: StateTimelineProps) {
    const containerRef = useRef<HTMLDivElement>(null);
    const canvasRef = useRef<HTMLCanvasElement>(null);

    const draw = useCallback(() => {
        const canvas = canvasRef.current;
        const container = containerRef.current;
        if (!canvas || !container || rows.length === 0) return;

        const ctx = canvas.getContext('2d');
        if (!ctx) return;

        const dpr = window.devicePixelRatio || 1;
        const width = container.clientWidth;
        const rowHeight = 24;
        const rowGap = 2;
        const labelWidth = 60;
        const height = rows.length * (rowHeight + rowGap);

        canvas.width = width * dpr;
        canvas.height = height * dpr;
        canvas.style.width = `${width}px`;
        canvas.style.height = `${height}px`;
        ctx.scale(dpr, dpr);

        // Clear
        ctx.fillStyle = '#1e1e1e';
        ctx.fillRect(0, 0, width, height);

        const timeRange = timeMax - timeMin;
        const barAreaWidth = width - labelWidth;

        // Draw each row
        for (let r = 0; r < rows.length; r++) {
            const row = rows[r];
            const y0 = r * (rowHeight + rowGap);

            // Draw label
            ctx.fillStyle = '#9cdcfe';
            ctx.font = '11px sans-serif';
            ctx.textBaseline = 'middle';
            ctx.textAlign = 'right';
            ctx.fillText(row.channel + ':', labelWidth - 8, y0 + rowHeight / 2);

            // Draw row background
            ctx.fillStyle = '#252526';
            ctx.fillRect(labelWidth, y0, barAreaWidth, rowHeight);

            // Draw segments
            for (const seg of row.segments) {
                const segStartSec = seg.start_ms / 1000;
                const segEndSec = seg.end_ms / 1000;

                // Skip if outside range
                if (segEndSec <= timeMin || segStartSec >= timeMax) continue;

                // Clamp to visible range
                const visStart = Math.max(segStartSec, timeMin);
                const visEnd = Math.min(segEndSec, timeMax);

                // Convert to pixels
                const x0 = labelWidth + ((visStart - timeMin) / timeRange) * barAreaWidth;
                const x1 = labelWidth + ((visEnd - timeMin) / timeRange) * barAreaWidth;
                const barWidth = Math.max(1, x1 - x0);

                // Draw bar
                ctx.fillStyle = getStateColor(seg.state);
                ctx.fillRect(x0, y0, barWidth, rowHeight);

                // Draw label if wide enough
                if (barWidth > 25) {
                    ctx.fillStyle = '#fff';
                    ctx.font = '11px sans-serif';
                    ctx.textBaseline = 'middle';
                    ctx.textAlign = 'left';
                    ctx.fillText(seg.state, x0 + 4, y0 + rowHeight / 2);
                }
            }
        }
    }, [rows, timeMin, timeMax]);

    useEffect(() => {
        draw();
    }, [draw]);

    useEffect(() => {
        const container = containerRef.current;
        if (!container) return;

        const resizeObserver = new ResizeObserver(() => {
            draw();
        });
        resizeObserver.observe(container);

        return () => resizeObserver.disconnect();
    }, [draw]);

    if (rows.length === 0) return null;

    return (
        <div ref={containerRef} className="state-timeline">
            <canvas ref={canvasRef} />
        </div>
    );
}
