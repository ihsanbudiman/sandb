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
// Soft pastels on the dark card: easier on the eye than full saturation.
const DARK = ['#e9c77f', '#7b9bdc', '#6cc4b0', '#a998e0', '#e98a9b', '#a9cf7a', '#72b8dc', '#d9b45a'];

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

export function chartOption(p: ChartPart, c: ChartColors, opts: { animation?: boolean; font?: string; mono?: string } = {}): EChartsCoreOption {
  const { spec, columns, rows } = p;
  const col = (name: string) => columns.indexOf(name);
  const xi = col(spec.x);
  const font = opts.font ?? 'Inter Variable, Helvetica, Arial, sans-serif';
  // Legend and axis labels use the mono face, as data reads better in it.
  const text = { color: c.muted, fontSize: 12, fontFamily: opts.mono ?? 'JetBrains Mono Variable, ui-monospace, monospace' };
  const legendStyle = { type: 'scroll', icon: 'roundRect', itemWidth: 10, itemHeight: 10, itemGap: 20, textStyle: text };
  const base = {
    animation: opts.animation ?? true,
    color: c.palette,
    textStyle: { fontFamily: font },
    title: spec.title ? { text: spec.title, left: 0, top: 0, padding: 0, textStyle: { color: c.ink, fontSize: 14, fontWeight: 'bold', fontFamily: font } } : undefined,
    tooltip: { trigger: spec.type === 'pie' || spec.type === 'scatter' ? 'item' : 'axis', axisPointer: { type: spec.type === 'bar' ? 'shadow' : 'line' }, valueFormatter: full, confine: true },
  };

  if (spec.type === 'pie') {
    const yi = col(spec.y[0]);
    return {
      ...base,
      legend: { ...legendStyle, bottom: 0 },
      series: [
        {
          type: 'pie',
          name: spec.y[0],
          radius: ['38%', '68%'],
          center: ['50%', spec.title ? '52%' : '46%'],
          label: { color: c.muted, fontSize: 11 },
          padAngle: 1,
          itemStyle: { borderColor: 'transparent', borderRadius: 4 },
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
  const header = Boolean(spec.title) || legend;
  const axisLine = { lineStyle: { color: c.line } };
  // Value axes only on scatter, where both axes carry data. Bar, line and area show the values in the tooltip.
  const valueAxis = { type: 'value', axisLabel: { ...text, formatter: (v: number) => compact.format(v) }, splitLine: { lineStyle: { color: c.line } }, axisLine };
  const hiddenAxis = { type: 'value', axisLabel: { show: false }, splitLine: { show: false }, axisLine: { show: false }, axisTick: { show: false } };
  const stack = spec.stacked && !scatter ? 'total' : undefined;
  const bar = spec.type === 'bar';
  return {
    ...base,
    legend: legend ? { ...legendStyle, top: 0, right: 0, padding: 0 } : undefined,
    grid: { left: 0, right: 0, top: header ? 52 : 12, bottom: 4, outerBoundsMode: 'same', outerBoundsContain: 'all' },
    xAxis:
      scatter
        ? { ...valueAxis, name: spec.x, nameLocation: 'middle', nameGap: 24, nameTextStyle: text, scale: true }
        : { type: 'category', data: categories, axisLabel: { ...text, hideOverlap: true, margin: 16 }, axisLine, axisTick: { show: false }, boundaryGap: bar },
    yAxis: scatter ? { ...valueAxis, scale: true } : hiddenAxis,
    series: [...named].map(([name, data]) => ({
      type: spec.type === 'area' ? 'line' : spec.type,
      name,
      stack,
      data,
      areaStyle: spec.type === 'area' ? { opacity: 0.2 } : undefined,
      lineStyle: scatter ? undefined : { width: 2.5 },
      showSymbol: scatter || categories.length <= 40,
      symbol: 'circle',
      symbolSize: scatter ? 7 : 6,
      // Round only the top of a bar; stacked segments meet flat.
      itemStyle: bar && !stack ? { borderRadius: [4, 4, 0, 0] } : undefined,
      barGap: '10%',
      barCategoryGap: '25%',
      barMaxWidth: 72,
    })),
  };
}

export function chartSvg(p: ChartPart, width: number, height: number): string {
  const chart = echarts.init(null, null, { renderer: 'svg', ssr: true, width, height });
  try {
    // The PDF has Helvetica built in; other fonts would fall back with a warning.
    chart.setOption(chartOption(p, PRINT_COLORS, { animation: false, font: 'helvetica', mono: 'helvetica' }));
    // svg2pdf knows only normal and bold; ECharts writes bold as 600.
    return chart.renderToSVGString().replace(/font-weight:\s*[6-9]00/g, 'font-weight:bold');
  } finally {
    chart.dispose();
  }
}
