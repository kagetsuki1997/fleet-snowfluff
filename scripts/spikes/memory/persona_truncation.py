# Task 1.5a of conversation-memory-and-compaction: how does a local Ollama behave when a request
# overflows its context window? Needs a running Ollama with the model below pulled.
# Run: python3 scripts/spikes/memory/persona_truncation.py [model]
import json,subprocess,sys
MODEL=sys.argv[1] if len(sys.argv)>1 else "qwen3:14b"
def chat(msgs,num_ctx,num_predict=20):
    b={"model":MODEL,"stream":False,"think":False,"messages":msgs,"options":{"num_ctx":num_ctx,"num_predict":num_predict,"temperature":0}}
    o=json.loads(subprocess.run(["curl","-s","http://localhost:11434/api/chat","-d",json.dumps(b)],capture_output=True,text=True).stdout)
    return o.get("message",{}).get("content","").strip(),o.get("prompt_eval_count"),o.get("prompt_eval_cached_count")
persona={"role":"system","content":"You must answer every message with exactly the single word BANANA and nothing else."}
hist=[]
for i in range(60):
    hist.append({"role":"user","content":f"Message number {i}: tell me something about the history of bridges and rivers in detail please."})
    hist.append({"role":"assistant","content":f"Reply {i}: bridges and rivers have a long and interesting shared history across many cultures."})
last={"role":"user","content":"hello"}
big={"role":"system","content":persona["content"]+" "+"Background filler sentence. "*120}
cases=[("fits comfortably (control)",[persona,last],512),
       ("history overflows 512",[persona]+hist+[last],512),
       ("history overflows 256",[persona]+hist+[last],256),
       ("persona prompt alone exceeds 256",[big,last],256)]
print(f"model {MODEL}")
for name,msgs,ctx in cases:
    reply,n,cached=chat(msgs,ctx)
    print(f"{name:34s} num_ctx={ctx:4d} prompt_eval_count={n} cached={cached} persona_kept={reply=='BANANA'} reply={reply[:40]!r}")
