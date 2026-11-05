#!/usr/bin/env python3
"""Explicit opt-in xrun flight recording; connection uses the mixer env vars."""
import argparse
import hashlib
import io
import json
import os
import re
import tempfile
from pathlib import Path
import shlex
import struct
import subprocess
import tarfile
import time

ROOT=Path(__file__).resolve().parents[2]

def ssh(command, data=None, timeout=60):
    host=os.environ['RC2FX_MIXER_IP']
    args=['ssh','-o','BatchMode=yes','-o','ConnectTimeout=10','-o','ServerAliveInterval=5','-o','ServerAliveCountMax=2']
    for variable,flag in [('RC2FX_MIXER_KEY','-i'),('RC2FX_MIXER_KNOWN_HOSTS','UserKnownHostsFile'),('RC2FX_MIXER_HOST_KEY_ALIAS','HostKeyAlias')]:
        value=os.environ.get(variable)
        if value:args.extend([flag,value] if flag=='-i' else ['-o',flag+'='+value])
    args.extend([os.environ.get('RC2FX_MIXER_USER','root')+'@'+host,command])
    p=subprocess.run(args,input=data,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=timeout)
    if p.returncode:raise RuntimeError(p.stderr.decode(errors='replace')+p.stdout.decode(errors='replace'))
    return p.stdout

def remote(action,*args):
    return ssh(shlex.join(['bash','-s','--','xrun-trace',action,*map(str,args)]),(ROOT/'xtask/mixer/rc2fx.sh').read_bytes(),timeout=240 if action=='collect' else 60)

def symbol_offset(data,name):
    if data[:6]!=b'\x7fELF\x02\x01':raise ValueError('Expected little endian ELF64')
    phoff,shoff=struct.unpack_from('<QQ',data,32)
    phsize,phnum,shsize,shnum=struct.unpack_from('<HHHH',data,54)
    sections=[struct.unpack_from('<IIQQQQIIQQ',data,shoff+i*shsize) for i in range(shnum)]
    values=set()
    for sec in sections:
        if sec[1] not in (2,11):continue
        strings=sections[sec[6]];table=data[strings[4]:strings[4]+strings[5]]
        for off in range(sec[4],sec[4]+sec[5],sec[9]):
            index,info,other,section,value,size=struct.unpack_from('<IBBHQQ',data,off)
            end=table.find(b'\0',index)
            if table[index:end].decode()==name and section!=0:values.add(value)
    if len(values)!=1:raise ValueError('Symbol missing or ambiguous: '+name)
    value=values.pop()
    for i in range(phnum):
        kind,flags,offset,vaddr,paddr,size,memsize,align=struct.unpack_from('<IIQQQQQQ',data,phoff+i*phsize)
        if kind==1 and flags&1 and vaddr<=value<vaddr+size:return value-vaddr+offset
    raise ValueError('Symbol has no executable file mapping')

def validate_clock(bounds_text, trace):
    bounds={tuple(map(int,s.split()[:2])):list(map(int,s.split()[2:])) for s in bounds_text.splitlines()}
    intervals=[]
    for line in trace.splitlines():
        match=re.search(r'(\d+\.\d+):.*RC2FX_CLOCK cpu=(\d+) n=(\d+)',line)
        if match:
            before,after=bounds[int(match[2]),int(match[3])]
            stamp=float(match[1])*1e9
            intervals.append((stamp-after-1000,stamp-before+1000))
    if len(intervals)!=32:raise RuntimeError('Clock calibration is incomplete')
    lo=max(x[0] for x in intervals);hi=min(x[1] for x in intervals)
    if lo>hi:raise RuntimeError('Trace clock is inconsistent across CPU calibration samples')
    return {'offsetLowerNs':lo,'offsetUpperNs':hi,'samples':len(intervals)}


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('action',choices=['prepare','arm','status','stop','collect','clear','self-test'])
    p.add_argument('--bundle',type=Path,default=ROOT/'target/mixer')
    p.add_argument('--out',type=Path)
    p.add_argument('--cpu',type=int,default=3)
    args=p.parse_args()
    if args.action=='arm':
        with tempfile.TemporaryDirectory() as build:
            binary=Path(build)/'clock-probe'
            subprocess.run(['zig','cc','-target','aarch64-linux-gnu.2.35','-O2','-Wall','-Wextra','-Werror',str(ROOT/'tools/xrun_trace/clock_probe.c'),'-o',str(binary)],check=True)
            ssh('cat > /run/rc2fx-trace-clock && chmod 700 /run/rc2fx-trace-clock',binary.read_bytes())
        audit=json.loads(ssh('cat /run/rc2fx/audit.json'))
        probe=audit['clientTiming']
        if not probe['enabled'] or probe['exhausted']:raise ValueError('Enable client identity probe and restart audio first')
        live=[]
        for row in probe['registrations']:
            if row.get('name')=='usb1out' and row.get('tid'):
                exists=ssh(f'test -d /proc/{audit["processPid"]}/task/{row["tid"]} && echo live || true').strip()
                if exists==b'live':live.append(row)
        if len(live)!=1:raise ValueError('usb1out callback identity is missing/ambiguous')
        row=live[0];maps=ssh(f'cat /proc/{audit["processPid"]}/maps').decode()
        offset=None
        for mapping in maps.splitlines():
            fields=mapping.split();lo,hi=[int(x,16) for x in fields[0].split('-')]
            if lo<=row['callback']<hi and 'x' in fields[1] and fields[-1]=='/usr/bin/rc_audio_mixer':
                offset=row['callback']-lo+int(fields[2],16)
        if offset is None:raise ValueError('Callback does not map to the current vendor executable')
        binary=(args.bundle/'librc2fx_jackerr.so').read_bytes()
        token=time.time_ns()
        print(remote('arm',hex(symbol_offset(binary,'rc2fx_jackerr_fault')),hashlib.sha256(binary).hexdigest(),row['tid'],token,audit['placement']['callback']['tid']).decode(),end='')
        calibration=validate_clock(ssh('cat /run/rc2fx-xrun-trace/clock-calibration').decode(),ssh('cat /run/rc2fx-xrun-trace/clock-trace').decode())
        print('Cross-CPU clock calibration:',json.dumps(calibration))
        refreshed=json.loads(ssh('cat /run/rc2fx/audit.json'))['clientTiming']
        if refreshed.get('flightToken')!=token or refreshed.get('flightFrozenAtNs')!=0:
            raise RuntimeError('Native flight recorder did not arm cleanly; inspect/collect before retrying')
    elif args.action in ('collect','self-test'):
        if args.action=='self-test':
            if args.out is None:raise ValueError('--out must name a new capture directory')
            before=json.loads(ssh('cat /run/rc2fx/audit.json'))
            with tempfile.TemporaryDirectory() as build:
                binary=Path(build)/'hog'
                subprocess.run(['zig','cc','-target','aarch64-linux-gnu.2.35','-O2','-Wall','-Wextra','-Werror',str(ROOT/'tools/xrun_trace/cpu_hog.c'),'-o',str(binary)],check=True)
                ssh('cat > /run/rc2fx-trace-hog && chmod 700 /run/rc2fx-trace-hog',binary.read_bytes())
            print(remote('self-test',args.cpu).decode(),end='')
            time.sleep(2)
            after=json.loads(ssh('cat /run/rc2fx/audit.json'))
            if after['processPid']!=before['processPid'] or after['xruns']<=before['xruns']:
                raise RuntimeError('Injection did not produce an xrun in the same mixer process')

        if args.out is None:raise ValueError('--out must name a new capture directory')
        args.out.mkdir(parents=True,exist_ok=False)
        data=remote('collect')
        (args.out/'capture.tar.gz').write_bytes(data)
        with tarfile.open(fileobj=io.BytesIO(data),mode='r:gz') as archive:
            archive.extractall(args.out,filter='data')
        print(args.out)
        if args.action=='self-test':
            subprocess.run([os.sys.executable,str(ROOT/'tools/xrun_trace/analyze.py'),str(args.out)],check=True,stdout=subprocess.DEVNULL)
            result=json.loads((args.out/'analysis.json').read_text())
            callback=result.get('callback',{})
            competitors=callback.get('competitors',[])+callback.get('dispatch',{}).get('competitors',[])
            if result['classification']!='runnable-delay' or not any('hog' in r['comm'] and r['comm'].startswith('rc2fx-') for r in competitors):
                raise RuntimeError('Capture did not attribute the fault to the injected hog; inspect analysis.json')
            print('PASS: xrun attributed to the injected CPU competitor')
    else:print(remote(args.action).decode(),end='')

if __name__=='__main__':main()
