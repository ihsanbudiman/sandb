// ECharts setup and the option for one chart part. The chat view and the PDF export share it.
import * as echarts from 'echarts/core';
import { BarChart, LineChart, PieChart, ScatterChart } from 'echarts/charts';
import { GridComponent, LegendComponent, TitleComponent, TooltipComponent } from 'echarts/components';
import { SVGRenderer } from 'echarts/renderers';
import type { EChartsCoreOption } from 'echarts/core';
import type { CellValue } from './api';
import type { ChartPart } from './chatModel';
import { formatCell } from './results';

echarts.use([BarChart, LineChart, PieChart, ScatterChart, GridComponent, LegendComponent, TitleComponent, TooltipComponent, SVGRenderer]);

export { echarts };

export interface ChartColors {
  ink: string;
  muted: string;
  line: string;
  palette: string[];
}

const LIGHT = ['#b45309', '#2563eb', '#0f766e', '#7c3aed', '#be123c', '#4d7c0f', '#0369a1', '#a16207'];
const DARK = ['#eda33c', '#60a5fa', '#2dd4bf', '#a78bfa', '#fb7185', '#a3e635', '#38bdf8', '#facc15'];

/** Light colors, for paper. */
export const PRINT_COLORS: ChartColors = { ink: '#1c1b19', muted: '#5f5b55', line: '#e5e3df', palette: LIGHT };

export function themeColors(dark: boolean): ChartColors {
  const css = getComputedStyle(document.documentElement);
  const v = (name: string) => css.getPropertyValue(name).trim();
  return { ink: v('--c-ink'), muted: v('--c-muted'), line: v('--c-line'), palette: dark ? DARK : LIGHT };
}

/** A number, or null. Exact numeric types (PG `numeric`, MySQL `DECIMAL`) arrive as text. */
function num(v: CellValue | undefined): number | null {
  if (typeof v === 'number') return v;
  if (typeof v === 'string' && /^\s*-?(\d+\.?\d*|\.\d+)(e[+-]?\d+)?\s*$/i.test(v)) return Number(v);
  return null;
}

const label = (v: CellValue | undefined) => (v === null || v === undefined ? 'NULL' : typeof v === 'object' ? formatCell(v) : String(v));
const compact = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 });
const full = (v: unknown) => (typeof v === 'number' ? v.toLocaleString() : String(v ?? ''));

export function chartOption(p: ChartPart, c: ChartColors, opts: { animation?: boolean; font?: string } = {}): EChartsCoreOption {
  const { spec, columns, rows } = p;
  const col = (name: string) => columns.indexOf(name);
  const xi = col(spec.x);
  const text = { color: c.muted, fontSize: 11 };
  const font = opts.font ?? 'Inter Variable, Helvetica, Arial, sans-serif';
  const base = {
    animation: opts.animation ?? true,
    color: c.palette,
    textStyle: { fontFamily: font },
    title: spec.title ? { text: spec.title, left: 0, top: 0, textStyle: { color: c.ink, fontSize: 13, fontWeight: 'bold', fontFamily: font } } : undefined,
    tooltip: { trigger: spec.type === 'pie' || spec.type === 'scatter' ? 'item' : 'axis', valueFormatter: full, confine: true },
  };

  if (spec.type === 'pie') {
    const yi = col(spec.y[0]);
    return {
      ...base,
      legend: { type: 'scroll', bottom: 0, textStyle: text },
      series: [
        {
          type: 'pie',
          name: spec.y[0],
          radius: ['38%', '68%'],
          center: ['50%', spec.title ? '52%' : '46%'],
          label: { color: c.muted, fontSize: 11 },
          itemStyle: { borderColor: 'transparent' },
          data: rows.map((r) => ({ name: label(r[xi]), value: num(r[yi]) })),
        },
      ],
    };
  }

  // One series per y column, or one per distinct value of the `series` column.
  const scatter = spec.type === 'scatter';
  const categories = scatter ? [] : [...new Set(rows.map((r) => label(r[xi])))];
  const at = new Map(categories.map((x, i) => [x, i]));
  const named = new Map<string, unknown[]>();
  const add = (name: string, r: CellValue[], yi: number) => {
    let points = named.get(name);
    if (!points) named.set(name, (points = scatter ? [] : categories.map(() => null)));
    if (scatter) points.push([num(r[xi]), num(r[yi])]);
    else points[at.get(label(r[xi]))!] = num(r[yi]);
  };
  for (const r of rows) {
    if (spec.series) add(label(r[col(spec.series)]), r, col(spec.y[0]));
    else for (const y of spec.y) add(y, r, col(y));
  }

  const legend = named.size > 1;
  const axisLine = { lineStyle: { color: c.line } };
  const splitLine = { lineStyle: { color: c.line } };
  const valueAxis = { type: 'value', axisLabel: { ...text, formatter: (v: number) => compact.format(v) }, splitLine, axisLine };
  const stack = spec.stacked && !scatter ? 'total' : undefined;
  return {
    ...base,
    legend: legend ? { type: 'scroll', bottom: 0, textStyle: text } : undefined,
    grid: { left: 8, right: 16, top: spec.title ? 40 : 16, bottom: legend ? 36 : 8, outerBoundsMode: 'same', outerBoundsContain: 'all' },
    xAxis:
      scatter
        ? { ...valueAxis, name: spec.x, nameLocation: 'middle', nameGap: 24, nameTextStyle: text, scale: true }
        : { type: 'category', data: categories, axisLabel: { ...text, hideOverlap: true }, axisLine, axisTick: { show: false }, boundaryGap: spec.type === 'bar' },
    yAxis: scatter ? { ...valueAxis, scale: true } : valueAxis,
    series: [...named].map(([name, data]) => ({
      type: spec.type === 'area' ? 'line' : spec.type,
      name,
      stack,
      data,
      areaStyle: spec.type === 'area' ? { opacity: 0.25 } : undefined,
      showSymbol: scatter || categories.length <= 40,
      symbolSize: scatter ? 7 : 5,
      barMaxWidth: 40,
    })),
  };
}

export function chartSvg(p: ChartPart, width: number, height: number): string {
  const chart = echarts.init(null, null, { renderer: 'svg', ssr: true, width, height });
  try {
    // The PDF has Helvetica built in; other fonts would fall back with a warning.
    chart.setOption(chartOption(p, PRINT_COLORS, { animation: false, font: 'helvetica' }));
    // svg2pdf knows only normal and bold; ECharts writes bold as 600.
    return chart.renderToSVGString().replace(/font-weight:\s*[6-9]00/g, 'font-weight:bold');
  } finally {
    chart.dispose();
  }
}
