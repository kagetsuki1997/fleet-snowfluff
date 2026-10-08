# Task 1.3 / 2.1 of conversation-memory-and-compaction: collect text samples and the prompt token
# counts a local Ollama model reports for them, as the fixture the Rust estimator is tested against.
# Needs a running Ollama. Run: python3 scripts/spikes/memory/token_calibration.py [model]
import json,os,subprocess,sys
MODEL=sys.argv[1] if len(sys.argv)>1 else "qwen3:14b"
OUT=os.path.join(os.path.dirname(os.path.abspath(__file__)),"..","..","..","crates","fleet-snowfluff-ai","tests","fixtures","memory","token_calibration.json")
def count(t):
    b={"model":MODEL,"stream":False,"think":False,"messages":[{"role":"system","content":"s"},{"role":"user","content":t}],"options":{"num_predict":1,"num_ctx":32768,"temperature":0}}
    o=json.loads(subprocess.run(["curl","-s","http://localhost:11434/api/chat","-d",json.dumps(b)],capture_output=True,text=True).stdout)
    return o["prompt_eval_count"]
S={
 "en_prose":"The quick brown fox jumps over the lazy dog while the river keeps running past the old stone bridge. "*8,
 "en_long":"Bridges and rivers have a long and interesting shared history across many cultures and centuries. "*60,
 "zh_hant":"今天想喝綠茶，麻糬又不吃飯了，週末想去彈吉他，姊姊的婚禮要準備什麼，搬家後還沒整理完。"*8,
 "zh_hans":"今天想喝绿茶，麻薯又不吃饭了，周末想去弹吉他，姐姐的婚礼要准备什么，搬家后还没整理完。"*8,
 "ja":"今日は緑茶が飲みたい。モチがごはんを食べない。週末にギターを弾きたい。姉の結婚式の準備。"*8,
 "ko":"오늘은 녹차가 마시고 싶다. 모찌가 밥을 안 먹어. 주말에 기타 치고 싶다. 누나 결혼식 준비는 어떻게 하지. "*8,
 "json":json.dumps({"results":[{"id":i,"title":f"Page {i}","url":f"https://example.com/p/{i}","tags":["a","b"]} for i in range(40)]}),
 "json_pretty":json.dumps({"results":[{"id":i,"title":f"Page {i}","props":{"x":i*3,"y":[1,2,3]}} for i in range(25)]},indent=2),
 "rust_code":'fn main() {\n    let v: Vec<u32> = (0..10).map(|x| x * x).collect();\n    for (i, n) in v.iter().enumerate() { println!("{i}: {n}"); }\n}\n'*6,
 "urls":"\n".join(f"https://www.example.com/path/{i}/article-title-{i}?utm_source=x&id={i*7919}" for i in range(30)),
 "log_lines":"\n".join(f"2026-10-08T12:{i%60:02d}:00Z INFO fleet::chat [req={i:06d}] completed in {i*3}ms status=200" for i in range(40)),
 "markdown_table":"| id | name | status |\n|----|------|--------|\n"+"\n".join(f"| {i} | item-{i} | ok |" for i in range(40)),
 "digits":" ".join(str(i*104729) for i in range(200)),
 "notion_md":"## Meeting notes\n\n- **Action**: ship the build by Friday\n- Owner: @alex\n- Link: [doc](https://notion.so/abc123def456)\n\n> quoted text here\n\n"*10,
}
base=count(".")
rows=[{"name":k,"text":t,"tokens":count(t)-base} for k,t in S.items()]
json.dump({"model":MODEL,"overhead_tokens_removed":base,"samples":rows},open(OUT,"w"),ensure_ascii=False,indent=1)
print(f"wrote {len(rows)} samples to {os.path.normpath(OUT)}")
