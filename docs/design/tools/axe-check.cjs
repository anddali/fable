// Runs axe-core (WCAG 2.0/2.1 A + AA rules) over every mockup in light and dark.
// Not part of the product build. One-off setup outside the repo:
//   mkdir /tmp/axe && (cd /tmp/axe && npm i puppeteer-core axe-core)
//   NODE_PATH=/tmp/axe/node_modules CHROME=/usr/bin/chromium node docs/design/tools/axe-check.cjs
const path = require('path');
const puppeteer = require('puppeteer-core');
const axeSrc = require('fs').readFileSync(require.resolve('axe-core/axe.min.js'), 'utf8');
const dir = path.resolve(__dirname, '..');
const pages = ['setup.html?step=1','setup.html?step=2','setup.html?step=3','setup.html?step=4','setup.html?step=5','setup.html?step=6','admin-model-providers.html','admin-model-roles.html','admin-integrations.html','source-confluence.html','collections.html','collection-detail.html','indexing-settings.html','search.html','chat.html','evaluation.html'];
(async () => {
  const b = await puppeteer.launch({executablePath: process.env.CHROME || '/usr/bin/chromium', args:['--no-sandbox','--allow-file-access-from-files']});
  let total=0;
  for (const theme of ['light','dark']) for (const p of pages) {
    const pg = await b.newPage(); await pg.setViewport({width:1440,height:1000});
    await pg.goto(`file://${dir}/${p}${p.includes('?')?'&':'?'}theme=${theme}`);
    await pg.addScriptTag({content: axeSrc});
    const r = await pg.evaluate(async () => (await axe.run(document, {runOnly:['wcag2a','wcag2aa','wcag21aa']})).violations.map(v => ({id:v.id, impact:v.impact, n:v.nodes.length, ex:v.nodes.slice(0,2).map(n=>n.target.join(' ')+' :: '+(n.failureSummary||'').split('\n')[1])})));
    if (r.length) { total+=r.length; console.log(theme, p, JSON.stringify(r,null,1)); }
    await pg.close();
  }
  console.log('violations:', total); await b.close(); process.exit(total ? 1 : 0);
})();
