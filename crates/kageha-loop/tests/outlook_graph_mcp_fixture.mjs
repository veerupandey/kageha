// Test harness: drive the real Outlook adapter against a disposable Graph fixture.
import {serve,readMessages} from '../../../integrations/outlook-verification/adapter.mjs';
const fixture = process.env.KAGEHA_GRAPH_FIXTURE_URL;
if (!/^http:\/\/127\.0\.0\.1:\d+$/.test(fixture || '')) throw Error('fixture URL required');
const fakeGraph = (url, options) => {
  const original = new URL(url);
  return fetch(fixture + original.pathname + original.search, options);
};
await serve(process.stdin, process.stdout, args => readMessages(args, fakeGraph, process.env.KAGEHA_GRAPH_ACCESS_TOKEN, process.env.KAGEHA_GRAPH_TRUSTED_AUTHSERV_ID));
