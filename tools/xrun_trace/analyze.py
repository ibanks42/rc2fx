"""Attribute a frozen ftrace window to observed execution states, not guesses."""
import argparse
import collections
import gzip
import json
import importlib.util
import re
from pathlib import Path

ROW = re.compile(r'^\s*(?P<comm>.+)-(?P<tid>\d+)\s+\[(?P<cpu>\d+)\]\s+(?:\S+\s+)?(?P<time>\d+\.\d+):\s+(?P<event>[\w:]+):\s*(?P<data>.*)$')
SWITCH = re.compile(r'prev_comm=(.*?) prev_pid=(\d+) prev_prio=\d+ prev_state=(\S+) ==> next_comm=(.*?) next_pid=(\d+)')
WAKE = re.compile(r'comm=(.*?) pid=(\d+).*target_cpu=(\d+)')
SYSCALL = re.compile(r'NR (\d+)')

def parse(text):
    rows = []
    for line in text.splitlines():
        m = ROW.match(line)
        if not m:
            continue
        row = m.groupdict()
        row.update(tid=int(row['tid']), cpu=int(row['cpu']), time=float(row['time']))
        row['event'] = row['event'].rsplit(':', 1)[-1]
        rows.append(row)
    return sorted(rows, key=lambda row: row['time'])

def callback_detail(rows, tid, start, end, initial_state='running'):
    state = initial_state
    cpu = start['cpu']
    current = {cpu: (tid, start['comm'])}
    for row in rows:
        if row['time'] >= start['time']:
            break
        if row['event'] == 'sched_switch':
            m = SWITCH.search(row['data'])
            if m:
                current[row['cpu']] = (int(m[5]), m[4])
    totals = collections.Counter()
    competitors = collections.Counter()
    names = {}
    pending_syscalls = {}
    syscalls = []
    wakes = []
    irq_start = {}
    irqs = []
    previous = start['time']
    for row in rows:
        if row['time'] < start['time'] or row['time'] > end:
            continue
        delta = max(0, row['time'] - previous)
        totals[state] += delta
        if state == 'runnable' and cpu in current:
            other, name = current[cpu]
            if other != tid:
                competitors[other] += delta
                names[other] = name
        previous = row['time']
        if current.get(row['cpu'], (None,))[0] == row['tid']:
            current[row['cpu']] = (row['tid'], row['comm'])
        event, data = row['event'], row['data']
        if event == 'sched_switch':
            m = SWITCH.search(data)
            if not m:
                continue
            _, prev, prev_state, name, nxt = m.groups()
            prev, nxt = int(prev), int(nxt)
            current[row['cpu']] = (nxt, name)
            if prev == tid:
                state = 'runnable' if prev_state.startswith('R') else 'blocked'
                cpu = row['cpu']
            if nxt == tid:
                state = 'running'
                cpu = row['cpu']
        elif event in ('sched_waking', 'sched_wakeup', 'sched_wakeup_new'):
            m = WAKE.search(data)
            if m and int(m[2]) == tid and state != 'running':
                state = 'runnable'
                cpu = int(m[3])
                wakes.append({'at': row['time'], 'wakerTid': row['tid'], 'waker': row['comm'], 'cpu': cpu})
        elif event == 'sys_enter' and row['tid'] == tid:
            m = SYSCALL.search(data)
            if m:
                pending_syscalls[tid] = (row['time'], int(m[1]), data)
        elif event == 'sys_exit' and row['tid'] == tid and tid in pending_syscalls:
            began, nr, args = pending_syscalls.pop(tid)
            syscalls.append({'nr': nr, 'start': began, 'end': row['time'], 'wallUs': (row['time']-began)*1e6, 'entry': args, 'exit': data})
        elif event in ('irq_handler_entry', 'softirq_entry'):
            key = (row['cpu'], event.split('_')[0])
            irq_start.setdefault(key, []).append((row['time'], data))
        elif event in ('irq_handler_exit', 'softirq_exit'):
            key = (row['cpu'], event.split('_')[0])
            if irq_start.get(key):
                began, description = irq_start[key].pop()
                irqs.append({'cpu': row['cpu'], 'start': began, 'end': row['time'], 'wallUs': (row['time']-began)*1e6, 'description': description})
    delta = max(0, end - previous)
    totals[state] += delta
    if state == 'runnable' and cpu in current and current[cpu][0] != tid:
        other, name = current[cpu]
        competitors[other] += delta
        names[other] = name
    for began, nr, args in pending_syscalls.values():
        syscalls.append({'nr': nr, 'start': began, 'end': None, 'wallUs': (end-began)*1e6, 'entry': args, 'incompleteAtFreeze': True})
    return {'tid': tid, 'start': start['time'], 'end': end, 'wallUs': (end-start['time'])*1e6,
            'onCpuIncludingIrqUs': totals['running']*1e6, 'runnableDelayUs': totals['runnable']*1e6,
            'blockedUs': totals['blocked']*1e6,
            'competitors': [{'tid': other, 'comm': names[other], 'observedCpuUs': seconds*1e6} for other, seconds in competitors.most_common()],
            'syscalls': syscalls, 'wakeups': wakes, 'interrupts': irqs}

def analyze(text, audit, stats='', fault_records=None, flight_audit=None, client_maps=None):
    rows = parse(text)
    limitations = []
    result = {'classification': 'insufficient-capture', 'limitations': limitations,
              'meaning': 'Observed scheduling/blocking mechanism; driver or application defect still requires source correlation.'}
    if re.search(r'(?:commit overrun|dropped events):\s*[1-9]', stats) or 'LOST EVENTS' in text:
        limitations.append('Trace reports lost events beyond normal overwrite of old history')
    faults = [r for r in rows if r['event'] == 'fault']
    if not faults and rows and fault_records is not None:
        # traceoff is executed before the kernel writes its triggering event.
        # The no-op hook follows publication to the independent JACK ring.
        matches = [r for r in fault_records if r.get('tNs') and r['kind'] != 'unknownFormat'
                   and r['tNs'] > audit.get('monotonicNs', 0)
                   and abs(r['tNs']/1e9-rows[-1]['time']) < 0.010]
        if matches:
            record = min(matches, key=lambda r:r['index'])
            faults = [{'time':record['tNs']/1e9,'event':'fault','tid':0,'cpu':-1,'comm':'JACK recorder',
                       'data':'client='+json.dumps(record.get('client', '')),'record':record}]
    if not faults:
        limitations.append('No JACK fault trigger event in capture')
        return result
    fault = faults[0]
    anchor = re.search(r'mono_ns=(\d+)', fault['data'])
    if anchor and int(anchor[1]):
        # global is cross-CPU ordered on this kernel, but has another epoch.
        # The hook's monotonic argument aligns the frozen window at the fault.
        offset = int(anchor[1])/1e9 - fault['time']
        for row in rows:
            row['time'] += offset
        result['clockAlignment'] = {'offsetSeconds':offset,
            'method':'fault hook CLOCK_MONOTONIC argument',
            'uncertainty':'User-to-probe latency; times are approximate, raw ordering is preserved'}
    result['fault'] = fault
    client = re.search(r'client="([^"]+)"', fault['data'])
    if not client:
        limitations.append('Fault has no readable JACK client name')
        return result
    tids = {r['tid'] for r in audit.get('clientTiming', {}).get('registrations', []) if r.get('name') == client[1] and r.get('tid')}
    if not tids and client_maps:
        ranges=[]
        for maps in client_maps.values():
            for mapping in maps.splitlines():
                fields=mapping.split()
                if fields and fields[-1].endswith('/jack_sem.0_default_'+client[1]):
                    lo,hi=map(lambda x:int(x,16),fields[0].split('-'));ranges.append((lo,hi))
        waiting={};active={};external=[]
        for row in rows:
            if row['event']=='sys_enter':
                m=re.search(r'NR 98 \(([0-9a-f]+), ([0-9a-f]+),',row['data'])
                if m and int(m[2],16)&127 in (0,9) and any(lo<=int(m[1],16)<hi for lo,hi in ranges):
                    if row['tid'] in active:
                        external.append(dict(row,event='callback_exit',data='external JACK semaphore cycle'))
                    waiting[row['tid']]=True;tids.add(row['tid'])
            elif row['event']=='sys_exit' and row['tid'] in waiting and 'NR 98 ' in row['data']:
                waiting.pop(row['tid'])
                active[row['tid']]=True
                external.append(dict(row,event='callback_enter',data='external JACK semaphore cycle'))
        rows.extend(external);rows.sort(key=lambda row:row['time'])
        if external:result['externalIdentity']='JACK client semaphore mapping and observed FUTEX_WAIT; interval includes JACK dispatch overhead'
    if len(tids) != 1:
        limitations.append('Client-to-thread identity is missing or ambiguous')
        return result
    tid = next(iter(tids))
    if flight_audit is not None:
        flight = flight_audit.get('clientTiming', {})
        if not flight.get('flightFrozenAtNs'):
            limitations.append('Native callback flight history did not freeze')
        for registration in flight.get('registrations', []):
            if registration.get('tid') != tid or not registration.get('flight'):
                continue
            history = registration['flight']
            recent = history['recent']
            if recent['dropped'] or recent['unavailableInWindow']:
                limitations.append('Native callback flight history reports unavailable rows')
            for began, ended, cycle, budget in recent['rows']:
                for when, event in [(began,'callback_enter'),(ended,'callback_exit')]:
                    if rows[0]['time'] <= when/1e9 <= fault['time']:
                        rows.append({'time':when/1e9,'event':event,'tid':tid,'cpu':-1,'comm':client[1],'data':'native flight recorder','estimatedCycleStart':cycle/1e9,'budgetSeconds':budget/1e9})
            began = history.get('activeStartNs', 0)
            if began and rows[0]['time'] <= began/1e9 <= fault['time']:
                rows.append({'time':began/1e9,'event':'callback_enter','tid':tid,'cpu':-1,'comm':client[1],'data':'in progress at native freeze'})
        rows.sort(key=lambda row:row['time'])
        # Native timestamps do not carry a CPU number. Recover it from the
        # scheduler timeline, rather than assuming the last observed affinity.
        running = {}
        for row in rows:
            if row['event']=='sched_switch':
                match=SWITCH.search(row['data'])
                if match:
                    running.pop(int(match[2]), None)
                    running[int(match[5])]=row['cpu']
            elif row['cpu']==-1:
                row['cpu']=running.get(row['tid'], -1)
    starts = []
    pairs = []
    for row in rows:
        if row['tid'] != tid or row['time'] > fault['time']:
            continue
        if row['event'] == 'callback_enter':
            starts.append(row)
        elif row['event'] == 'callback_exit' and starts:
            pairs.append((starts.pop(), row['time'], True))
    pairs.extend((start, fault['time'], False) for start in starts)
    # Preserve neighboring callbacks: the server may report the previous cycle
    # after a later callback has entered. Do not silently select a fresh one.
    pairs = [p for p in pairs if fault['time'] - p[1] <= 0.010]
    # A deadline can expire before the callback gets to enter at all. Keep
    # the latest wake-to-fault interval independently of completed callbacks.
    latest_wake = None
    for row in rows:
        if row['time'] > fault['time']:
            break
        if row['event'] in ('sched_waking', 'sched_wakeup', 'sched_wakeup_new'):
            match = WAKE.search(row['data'])
            if match and int(match[2]) == tid:
                latest_wake = dict(row, cpu=int(match[3]))
    last_callback_end = max((p[1] for p in pairs), default=0)
    pending_dispatch = None
    if latest_wake and latest_wake['time'] > last_callback_end:
        pending_dispatch = callback_detail(rows, tid, latest_wake, fault['time'], 'runnable')
        result['pendingDispatch'] = pending_dispatch
    if not pairs and pending_dispatch is None:
        limitations.append('No recent callback interval for the named client')
        return result
    details = []
    for start, end, completed in pairs:
        detail = callback_detail(rows, tid, start, end)
        wake = None
        for row in rows:
            if row['time'] > start['time']:
                break
            if row['event'] in ('sched_waking', 'sched_wakeup', 'sched_wakeup_new'):
                match = WAKE.search(row['data'])
                if match and int(match[2]) == tid:
                    wake = dict(row, cpu=int(match[3]))
        previous_end=max((p[1] for p in pairs if p[1]<start['time']),default=0)
        if wake and wake['time']>=previous_end and start['time'] - wake['time'] < 0.100:
            detail['dispatch'] = callback_detail(rows, tid, wake, start['time'], 'runnable')
        if start.get('estimatedCycleStart'):
            cycle=start['estimatedCycleStart'];budget=start['budgetSeconds']
            detail['estimatedCycleStart']=cycle
            detail['cycleBudgetUs']=budget*1e6
            detail['entryPositionUs']=(start['time']-cycle)*1e6
            detail['remainingAtEntryUs']=(cycle+budget-start['time'])*1e6
        detail['completedAtFreeze'] = completed
        details.append(detail)
    if pending_dispatch is not None:
        details.append(dict(pending_dispatch, completedAtFreeze=False, phase='before-callback-entry'))
    detail = max(details, key=lambda row: row['end'])
    result['callback'] = detail
    result['nearbyCallbacks'] = details
    main_tid=(flight_audit or {}).get('placement',{}).get('callback',{}).get('tid')
    main=(flight_audit or {}).get('clientTiming',{}).get('mainFlight') or {}
    main_rows=main.get('recent',{}).get('events',[])
    token=(flight_audit or {}).get('clientTiming',{}).get('flightToken')
    main_rows=[r for r in main_rows if r.get('registrationTag')==token and 0 <= fault['time']-r['startNs']/1e9 < .010]
    if main_tid and main_rows:
        prior=max(main_rows,key=lambda r:r['startNs'])
        began=prior['startNs']/1e9;ended=min(prior['endNs']/1e9,fault['time'])
        first=next((r for r in rows if r['tid']==main_tid and r['time']>=began),None)
        if first:
            result['upstreamMainCallback']=callback_detail(rows,main_tid,dict(time=began,cpu=first['cpu'],comm='main callback'),ended)
            result['upstreamMainCallback']['nativeWorkUs']=(prior['nativeEndNs']-prior['nativeStartNs'])/1000 if prior.get('nativeStartNs') and prior.get('nativeEndNs') else None
    result['traceRange'] = [rows[0]['time'], rows[-1]['time']]
    if limitations:
        return result
    if detail['blockedUs'] > max(100, detail['runnableDelayUs']) and detail['wallUs'] > 2333.333:
        result['classification'] = 'blocking-syscall' if detail['syscalls'] else 'blocked-without-syscall-evidence'
    elif detail['runnableDelayUs'] + detail.get('dispatch', {}).get('runnableDelayUs', 0) > 100:
        result['classification'] = 'runnable-delay'
    elif detail['wallUs'] > 2333.333:
        result['classification'] = 'on-cpu-or-irq'
    elif detail.get('entryPositionUs',0) > detail.get('cycleBudgetUs',1)*.75:
        result['classification']='upstream-cycle-budget-exhausted'
        result['meaning']='USB callback began after upstream work consumed most of its cycle; inspect upstream main callback and wake chain for the source of that delay.'
    else:
        limitations.append('Observed callback execution was within one period; upstream cycle/wakeup dependency needs examination')
    return result

if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('capture', type=Path)
    args = parser.parse_args()
    root = args.capture
    with gzip.open(root/'trace.txt.gz', 'rt') as stream:
        text = stream.read()
    decoder_path=Path(__file__).resolve().parents[2]/'crates/rc2fx-native/jackerr/tests/jackerr_ring.py'
    spec=importlib.util.spec_from_file_location('jackerr_ring',decoder_path)
    decoder=importlib.util.module_from_spec(spec);spec.loader.exec_module(decoder)
    records=decoder.read((root/'jack.snapshot').read_bytes())['records'] if (root/'jack.snapshot').exists() else None
    result = analyze(text, json.loads((root/'arm-audit.json').read_text()), (root/'buffer-stats').read_text(), records,
                     json.loads((root/'collected-audit.json').read_text()),
                     {p.name:p.read_text() for p in root.glob('jack-client-maps.*')})
    if (root/'profile').exists() and (root/'profile').read_text().strip()=='minimal':
        result['schedulerScope']='Critical thread transitions only; intermediate competitors may be missing. Competitor interval is not proof of continuous CPU occupancy.'
        def scope(value):
            if isinstance(value,dict):
                if 'observedCpuUs' in value:value['intervalSinceObservedSwitchUs']=value.pop('observedCpuUs')
                for child in value.values():scope(child)
            elif isinstance(value,list):
                for child in value:scope(child)
        scope(result)
    (root/'analysis.json').write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps(result, indent=2))
