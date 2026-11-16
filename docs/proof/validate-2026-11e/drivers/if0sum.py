import re,sys,collections
for f in sys.argv[1:]:
    n=collections.Counter(); s=collections.Counter(); m={}
    for l in open(f,errors="replace"):
        l=l.replace("\r","")
        mm=re.match(r"\[if0-window\] (\d+) ms cause=(\S+)( \d+)?.*?task (\d+)",l)
        if not mm: continue
        ms=int(mm.group(1)); c=mm.group(2)+(mm.group(3) or "")
        if c=="kernel": c+=" t"+mm.group(4)
        n[c]+=1; s[c]+=ms; m[c]=max(m.get(c,0),ms)
    print("==",f, "total", sum(s.values()))
    for c,_ in s.most_common(6): print("   %-20s n=%3d sum=%6d max=%5d"%(c,n[c],s[c],m[c]))
