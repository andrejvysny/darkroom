// Record directly from the production TypeScript; no copied implementation.
import { build } from "esbuild";
import { readFile, writeFile } from "node:fs/promises";

const tone = await readFile("src/views/Develop/ToneCurve.tsx", "utf8");
const entry = `
import * as frame from './src/lib/frame';
import * as mask from './src/lib/maskGeom';
${tone.replace('export default function ToneCurve', 'function ToneCurve')}
export { frame, mask, cleanPts, evalClean };
`;
const result = await build({ stdin: { contents: entry, resolveDir: process.cwd(), loader: "tsx" }, bundle: true, format: "esm", platform: "node", write: false });
const { frame, mask, cleanPts, evalClean } = await import(`data:text/javascript;base64,${Buffer.from(result.outputFiles[0].text).toString("base64")}`);
const border = (size, aspectW = 0, aspectH = 0) => ({ size, aspectW, aspectH, color: [1, 1, 1] });
const frames = [[100,50,border(0)], [100,80,border(0.49999999999999994)], [101,50,border(2.5,1,1)], [50,100,border(-2,16,9)], [0,0,border(0,1,1)], [10,4,border(5,0,1)], [500,400,border(0.7)], [1,19,border(0,1.5/19,1)]]
  .map(([w,h,border]) => ({ w,h,border, result: frame.computeFrame(w,h,border) }));
const rects = [[10,20,100,50,60,45], [0,0,0,-1,2,-2], [10,20,10,10,0,50]]
  .map(([left,top,width,height,x,y]) => ({ rect: {left,top,width,height}, x,y,result: mask.clientToNorm({left,top,width,height},x,y) }));
const colors = [[1,0,0], [0,1,0], [0,0,1], [1,0,0.5], [0,0,0], [0.000001,0,0], [-1,2,0.5]]
  .map(rgb => ({rgb,hsv: mask.rgbToHsv(...rgb),css: frame.rgb3ToCss(rgb)}));
const masks = [mask.newLinearMask(),mask.newRadialMask(),mask.newBrushMask(),mask.newAiMask(),mask.newLuminanceMask(),mask.newColorMask()]
  .map(value => ({value,label: mask.maskKindLabel(value)}));
const curveInputs = [[], [{x:0.5,y:0.1}], [{x:0,y:0},{x:1,y:1}], [{x:1,y:1},{x:0,y:0},{x:0.5,y:0.8}], [{x:0,y:1},{x:0.3,y:0.2},{x:0.7,y:0.8},{x:1,y:0}], [{x:0,y:0},{x:0,y:1},{x:1,y:1}], [{x:-1,y:2},{x:1,y:0}], [{x:0.5,y:0},{x:0.50000099,y:1},{x:1,y:1}], [{x:0.5,y:0},{x:0.50000101,y:1},{x:1,y:1}]];
const curves = curveInputs.map(points => ({points, samples: [-0.1,0,0.125,0.5,0.50000099,0.50000101,0.9,1,1.1].map(x => ({x,y:evalClean(cleanPts(points),x)}))}));
const special = { clampNaN: String(mask.clamp01(NaN)), hsvNaN: mask.rgbToHsv(NaN,0,1).map(String), cssNaN: frame.rgb3ToCss([NaN, Infinity, -Infinity]), curveNaN: String(evalClean(cleanPts([{x:NaN,y:1},{x:0,y:0},{x:1,y:1}]), NaN)) };
await writeFile("crates/core-model/tests/fixtures/geometry.json", JSON.stringify({frames,rects,colors,masks,curves,special}, null, 2)+"\n");
