# Throwaway spike for task 1.1 of conversation-memory-and-compaction. Not shipped.
# Run: python3 scripts/spikes/memory/recall_spike.py [-v]
# Throwaway prototype of the planned keyword matching: script-aware tokenizer, optional
# Traditional->Simplified normalization, FTS5 + bm25, evaluated on the checked-in fixtures.
import json,re,sqlite3,sys,unicodedata,collections
import os,urllib.request
HERE=os.path.dirname(os.path.abspath(__file__))
FIX=os.path.join(HERE,"..","..","..","crates","fleet-snowfluff-ai","tests","fixtures","memory")
CACHE=os.path.join(os.environ.get("TMPDIR","/tmp"),"opencc-TSCharacters.txt")
if not os.path.exists(CACHE):
    urllib.request.urlretrieve("https://raw.githubusercontent.com/BYVoid/OpenCC/master/data/dictionary/TSCharacters.txt",CACHE)
T2S={}
for line in open(CACHE,encoding="utf8"):
    if line.startswith("#") or "\t" not in line: continue
    k,v=line.rstrip("\n").split("\t"); T2S[k]=v.split()[0]
STOP=set("a an the and or but if of to in on at for with by from as is are was were be been am do does did you your i me my we our it its this that these those what which who how when where why can could should would will shall may might not no so than then too very just about into out up down over under again here there some any each more most other such only own same also".split())
FLOOR=-2.2
def is_cjk(c):
    o=ord(c)
    return (0x4E00<=o<=0x9FFF or 0x3400<=o<=0x4DBF or 0xF900<=o<=0xFAFF or
            0x3040<=o<=0x30FF or 0x31F0<=o<=0x31FF or 0xAC00<=o<=0xD7AF or 0x1100<=o<=0x11FF or 0x3130<=o<=0x318F)
def norm(text,t2s):
    text=unicodedata.normalize("NFKC",text).lower()
    return "".join(T2S.get(c,c) for c in text) if t2s else text
def tokens(text,t2s):
    text=norm(text,t2s); out=[]; i=0
    while i<len(text):
        c=text[i]
        if is_cjk(c):
            j=i
            while j<len(text) and is_cjk(text[j]): j+=1
            run=text[i:j]
            out+= [run[k:k+2] for k in range(len(run)-1)] if len(run)>1 else []
            i=j
        elif c.isalnum():
            j=i
            while j<len(text) and text[j].isalnum() and not is_cjk(text[j]): j+=1
            w=text[i:j]
            if w not in STOP: out.append(w)
            i=j
        else: i+=1
    return out
def single_cjk(text,t2s):
    text=norm(text,t2s); res=[]; i=0
    while i<len(text):
        if is_cjk(text[i]):
            j=i
            while j<len(text) and is_cjk(text[j]): j+=1
            if j-i==1: res.append(text[i])
            i=j
        else: i+=1
    return res
def build(mems,t2s):
    db=sqlite3.connect(":memory:")
    db.execute("create virtual table f using fts5(body,tokenize='porter unicode61')")
    db.execute("create table m(rowid integer primary key,id text,norm text)")
    for n,m in enumerate(mems,1):
        full=m["text"]+" "+" ".join(m.get("tags",[]))
        db.execute("insert into f(rowid,body) values(?,?)",(n,' '.join(tokens(full,t2s))))
        db.execute("insert into m values(?,?,?)",(n,m["id"],norm(full,t2s)))
    return db
def search(db,q,t2s,k=5,floor=FLOOR):
    toks=tokens(q,t2s); hits=[]
    if toks:
        expr=" OR ".join('"'+t.replace('"','')+'"' for t in toks)
        for rid,score in db.execute("select rowid,bm25(f) from f where f match ? order by bm25(f) limit ?",(expr,k)):
            if floor is None or score<=floor: hits.append((rid,score))
    ids=[db.execute("select id from m where rowid=?",(r,)).fetchone()[0] for r,_ in hits]
    if len(ids)<k:   # one-character fallback: substring scan of the normalized text
        for ch in single_cjk(q,t2s):
            for (mid,) in db.execute("select id from m where norm like ?",(f"%{ch}%",)):
                if mid not in ids and len(ids)<k: ids.append(mid)
    return ids,[s for _,s in hits]
def evaluate(mem_lang,q_lang,t2s,floor=FLOOR):
    mems=json.load(open(f"{FIX}/recall_{mem_lang}.json"))["memories"]
    qs=json.load(open(f"{FIX}/recall_{q_lang}.json"))["queries"]
    db=build(mems,t2s); by=collections.defaultdict(lambda:[0,0]); neg=[0,0]; misses=[]
    for q in qs:
        ids,_=search(db,q["text"],t2s,floor=floor)
        if q["kind"]=="negative":
            neg[1]+=1; neg[0]+= 1 if ids else 0
        else:
            ok=any(e in ids for e in q["expect"]); by[q["kind"]][1]+=1; by[q["kind"]][0]+=ok
            if not ok: misses.append((q["kind"],q["text"]))
    return by,neg,misses
def fmt(by,neg):
    pos=[sum(v[0] for k,v in by.items() if k!="paraphrase"),sum(v[1] for k,v in by.items() if k!="paraphrase")]
    s=f"non-paraphrase recall@5 {pos[0]}/{pos[1]}"
    if "paraphrase" in by: s+=f" | paraphrase {by['paraphrase'][0]}/{by['paraphrase'][1]}"
    s+=f" | negatives matched {neg[0]}/{neg[1]}"
    return s
if __name__=="__main__":
    print("== same-language, normalization OFF vs ON")
    for lang in ["en","zh-Hant","zh-Hans","ja","ko"]:
        for t2s in (False,True):
            by,neg,miss=evaluate(lang,lang,t2s)
            print(f"{lang:8s} norm={'on ' if t2s else 'off'} {fmt(by,neg)}")
        if "-v" in sys.argv:
            for k,t in evaluate(lang,lang,False)[2]: print("    miss:",k,t)
    print("== cross-script (query script != memory script)")
    for ml,ql in (("zh-Hans","zh-Hant"),("zh-Hant","zh-Hans")):
        for t2s in (False,True):
            by,neg,_=evaluate(ml,ql,t2s)
            print(f"memories {ml} / queries {ql} norm={'on ' if t2s else 'off'} {fmt(by,neg)}")
