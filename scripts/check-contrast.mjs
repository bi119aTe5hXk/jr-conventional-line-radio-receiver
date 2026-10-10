// Declared token pairs, not a substitute for checking rendered/composited colors.
import { readFile } from "node:fs/promises";
const css = await readFile(new URL("../ui/style.css", import.meta.url), "utf8");
const tokens = Object.fromEntries([...css.matchAll(/(--[\w-]+):\s*([^;]+);/g)].map(m => [m[1], m[2].trim()]));
const resolve = name => tokens[name].startsWith("var(") ? resolve(tokens[name].slice(4,-1)) : tokens[name];
const luminance = hex => {
  const rgb = [1,3,5].map(i => parseInt(hex.slice(i,i+2),16)/255).map(v => v<=.04045 ? v/12.92 : ((v+.055)/1.055)**2.4);
  return rgb[0]*.2126+rgb[1]*.7152+rgb[2]*.0722;
};
const pairs = [];
function check(foreground,background,threshold,role) {
  const fg = resolve(foreground), bg = resolve(background);
  const l = [luminance(fg),luminance(bg)].sort((a,b)=>b-a);
  const ratio = (l[0]+.05)/(l[1]+.05);
  pairs.push({role,foreground,background,fg,bg,ratio:Number(ratio.toFixed(2)),threshold,pass:ratio>=threshold});
}
for (const surface of ["--bg","--surface","--surface-raised","--control","--selected"]) {
  for (const text of ["--text","--muted","--accent","--success","--danger"]) check(text,surface,4.5,"text");
  check("--focus",surface,3,"focus perimeter");
}
check("--on-accent","--accent",4.5,"primary action");
check("--success","--success-bg",4.5,"receiving state");
check("--danger","--danger-bg",4.5,"error");
check("--danger","--surface",4.5,"inline error");
for (const surface of ["--bg","--surface","--control","--selected"]) check("--control-border",surface,3,"control boundary");
check("--plot-signal","--inset",3,"constellation point");
check("--plot-grid","--inset",3,"constellation reference");
console.log(JSON.stringify({scope:"Declared opaque token pairs; disabled controls excluded; actual rendering not verified",pairs},null,2));
if (pairs.some(p=>!p.pass)) process.exitCode = 1;
