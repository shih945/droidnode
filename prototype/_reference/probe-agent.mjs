// Read-only probe of the on-device agent's loopback API through `adb forward tcp:11255 tcp:10255`.
// Writes each JSON payload next to this script. No state is changed: GET only.
import https from 'node:https';
import { writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const agent = new https.Agent({ rejectUnauthorized: false, keepAlive: false });

function get(path) {
  return new Promise((resolve, reject) => {
    const req = https.get(
      { host: '127.0.0.1', port: 11255, path, agent, timeout: 8000 },
      (res) => {
        let body = '';
        res.setEncoding('utf8');
        res.on('data', (c) => (body += c));
        res.on('end', () => resolve({ status: res.statusCode, body }));
      },
    );
    req.on('timeout', () => req.destroy(new Error('timeout')));
    req.on('error', reject);
  });
}

const targets = [
  ['healthz', '/healthz'],
  ['pods', '/pods'],
  ['images', '/images'],
  ['stats-summary', '/stats/summary'],
  ['metrics-resource', '/metrics/resource'],
];

for (const [name, path] of targets) {
  try {
    const r = await get(path);
    const file = join(here, 'current-ui', `api-${name}.json`);
    writeFileSync(file, r.body);
    console.log(`${path} -> ${r.status} (${r.body.length} bytes) -> api-${name}.json`);
  } catch (e) {
    console.log(`${path} -> ERROR ${e.message}`);
  }
}
