// Playwright CLI run-code fixture. Open an authenticated local test chat first.
// All chat submissions are intercepted; saved conversations are never modified.
async (page) => {
  const current = new URL(page.url());
  if (!['127.0.0.1', 'localhost'].includes(current.hostname)) throw new Error('Use a local test server');
  const sid = current.searchParams.get('session');
  if (!sid) throw new Error('Open an existing local test session first');
  const url = `${current.origin}/?session=${sid}`;
  const template = await (await page.request.get(`${current.origin}/api/sessions/${sid}`)).json();
  let scenario = null;
  const results = [];
  const errors = [];
  const networkErrors = [];
  await page.unroute('**/api/chat/stream');
  await page.unroute(`**/api/sessions/${sid}**`);
  const onPageError = error => errors.push(String(error));
  page.on('pageerror', onPageError);
  const onResponse = response => { if(response.status() >= 400) networkErrors.push({url:response.url(),status:response.status()}); };
  page.on('response', onResponse);
  const snapshot = () => ({...template, title:'Answer delivery regression', status:'success',
    active_turn:null, live_message:'', live_user_message:'', pending_approval:null,
    messages:scenario.finished ? [
      {role:'user',text:'Check the rate',turn_id:scenario.turn},
      {role:'assistant',text:scenario.final,turn_id:scenario.turn}]
    : [],
    turns:scenario.finished ? [{id:scenario.turn,objective:'Check the rate',status:'success',phase:'completed'}] : []});
  try {
  await page.route(`**/api/sessions/${sid}**`, async route => {
    const path = new URL(route.request().url()).pathname;
    if (path.includes('/events/stream')) {
      const update = {events:scenario.events,live:snapshot()};
      return route.fulfill({contentType:'text/event-stream',body:`event: update\ndata: ${JSON.stringify(update)}\n\n`});
    }
    if (path === `/api/sessions/${sid}/events`) return route.fulfill({json:{events:scenario.events,live:snapshot()}});
    if (path === `/api/sessions/${sid}` && route.request().method()==='GET') return route.fulfill({json:snapshot()});
    return route.continue();
  });
  await page.route('**/api/chat/stream', async route => {
    const body = route.request().postDataJSON();
    if (body.session_id !== sid) throw new Error('Unexpected session');
    scenario.finished = true;
    const frame = (event,data) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
    const frames = [frame('status',{phase:'received',turn_id:scenario.turn,session_id:sid,thread_id:template.thread_id}),
      frame('delta',{text:scenario.prefix+(scenario.name==='partial-draft' ? scenario.original.slice(0,10) : scenario.original)})];
    for (const event of scenario.events.filter(e=>e.kind!=='completed' && scenario.name!=='drop-before-revision')) frames.push(frame('event',event));
    if (!scenario.drop) {
      frames.push(frame('message',{text:scenario.final}));
      frames.push(frame('done',{status:'success',message:scenario.final,turn_id:scenario.turn,session_id:sid}));
    }
    return route.fulfill({contentType:'text/event-stream',body:frames.join('')});
  });
  for (const [name,original,final,drop] of [
    ['unchanged','The corrected value is 3%.','The corrected value is 3%.',false],
    ['unverified','The corrected value is 3%.','The corrected value is 3%.\n\nVerification remains incomplete.',false],
    ['shorter','The corrected value is 3%, with an unsupported qualifier.','The corrected value is 4%.',false],
    ['longer-unicode','The corrected value is 3%.','The corrected value is 4%. ✅\n\nA source was checked. Freshness is unverified.',false],
    ['partial-draft','The corrected value is 3%, with an unsupported qualifier.','The corrected value is 4%.',false],
    ['drop-before-revision','The corrected value is 3%, with an unsupported qualifier.','The corrected value is 4%.',true],
    ['drop-after-revision','The corrected value is 3%, with an unsupported qualifier.','The corrected value is 4%.',true],
  ]) {
    scenario={name,original,final,drop,prefix:'Checking the source.\n\n',turn:`regression-${name}`,finished:false,events:[]};
    const source={sequence:1,kind:'progress',payload:{source_kind:'assistant_narrative',text:scenario.prefix+original,offset:0}};
    const revision={sequence:2,kind:'progress',payload:{source_kind:'answer_revision',original,replacement:final,offset:scenario.prefix.length}};
    scenario.events=[...(name==='partial-draft' ? [] : [source]),...(original===final ? [] : [revision]),{...source,sequence:3},...(original===final ? [] : [{...revision,sequence:4}]),
      {sequence:5,kind:'completed',payload:{status:'success',message:final}}];
    await page.goto(url);
    await page.getByRole('textbox',{name:'Message Kageha'}).fill('Check the rate');
    await page.getByRole('button',{name:'Send',exact:true}).click();
    const assistant=page.getByRole('region',{name:'Conversation'}).locator('article').last();
    await assistant.getByText(final.split('\n')[0],{exact:false}).first().waitFor({timeout:20000});
    const check = async phase => {
      const text=await assistant.innerText();
      if ((!final.includes('3%') && text.includes('3%')) || text.split(final.split('\n')[0]).length!==2 || text.includes('"corrections"')) throw new Error(`${name}/${phase}: repeated or leaked answer: ${text}`);
      results.push(`${name}/${phase}: PASS`);
    };
    await check('live');
    await page.reload();
    await assistant.getByText(final.split('\n')[0],{exact:false}).first().waitFor({timeout:20000});
    await check('refresh');
  }
  await page.setViewportSize({width:1440,height:1000});
  await page.screenshot({path:'output/playwright/repeat-regression-desktop.png'});
  await page.setViewportSize({width:390,height:844});
  await page.screenshot({path:'output/playwright/repeat-regression-mobile.png'});
  if (errors.length) throw new Error(errors.join('\n'));
  await page.unroute('**/api/chat/stream');
  await page.unroute(`**/api/sessions/${sid}**`);
  const deliveryFailures = networkErrors.filter(error => {
    const path = new URL(error.url).pathname;
    return path === '/api/chat/stream' || path === `/api/sessions/${sid}/events` || path === `/api/sessions/${sid}/events/stream`;
  });
  if (deliveryFailures.length) throw new Error(JSON.stringify(deliveryFailures));
  return {results,pageErrors:errors.length,deliveryFailures,backgroundNetworkErrors:networkErrors};
  } finally {
    await page.unroute('**/api/chat/stream');
    await page.unroute(`**/api/sessions/${sid}**`);
    page.off('pageerror', onPageError);
    page.off('response', onResponse);
  }
}
