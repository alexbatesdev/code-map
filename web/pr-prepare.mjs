import { createInterface } from 'node:readline';
import { prepareComparison } from './pr-snapshots.mjs';
let used = false;
createInterface({ input: process.stdin }).on('line', async line => {
  let request;
  try {
    if (used) throw new Error('PR preparation accepts one request'); used = true;
    request = JSON.parse(line);
    const result = await prepareComparison(request.project, request.number, request.directory);
    process.stdout.write(JSON.stringify({ id: request.id, result }) + '\n');
  } catch (error) { process.stdout.write(JSON.stringify({ id: request?.id ?? null, error: String(error.message).slice(0, 2048) }) + '\n'); }
});
