# Task 1.4 of conversation-memory-and-compaction: draft and test the three background prompts on a
# local model. Needs a running Ollama. Run: python3 scripts/spikes/memory/prompts_spike.py [model]
import json,re,subprocess,sys
MODEL=sys.argv[1] if len(sys.argv)>1 else "qwen3:14b"
LANG_NAME={"zh-Hant":"Traditional Chinese (繁體中文)","zh-Hans":"Simplified Chinese (简体中文)","en":"English","ja":"Japanese (日本語)","ko":"Korean (한국어)"}
def summary_system(lang):
    return ("You keep neutral background notes about an ongoing conversation between a user and an assistant. "
 "You are given the existing notes (possibly empty) and newer messages. Write updated notes that merge both.\n"
 "Rules:\n- Plain third-person notes. Never write in the assistant's voice or any character's voice.\n"
 "- Keep facts, plans, decisions and open questions. Drop greetings, small talk and the weather.\n"
 "- Leave out anything about health, medication, money or debt, passwords or credentials, sexuality, religion, or politics.\n"
 "- Never invent anything that is not in the messages.\n"
 f"- Write the notes in {LANG_NAME[lang]}, whatever language these instructions are in.\n"
 "- At most 150 words. Output only the notes.")
EXTRACT_SYSTEM=("From the conversation, list durable facts about the USER that would still be useful in later conversations: "
 "preferences, pets, work, family, plans, habits, important dates.\n"
 "Return ONLY a JSON array, possibly empty. Each item: {\"text\": a short statement without the subject (at most 20 words), "
 "\"kind\": \"preference\" | \"life_context\" | \"shared_moment\" | \"other\", \"importance\": 1, 2 or 3, "
 "\"tags\": 2 to 4 keywords, in the user's language and in English}.\n"
 "Rules:\n- Do NOT include anything about health, medication, money or debt, passwords or credentials, sexuality, religion, or politics, even if it was mentioned.\n"
 "- Skip temporary states (what they ate today, today's mood) and anything the assistant said.\n"
 "- Do not repeat anything listed under Already known.\n- If nothing qualifies, return [].")
CORRECT_SYSTEM=("The user may be correcting something stored about them. Decide which stored memory, if any, the new fact replaces "
 "(same subject, now outdated). Return ONLY JSON: {\"replaces\": \"<id>\" or null, \"text\": \"<the corrected memory, short>\"}. "
 "Only choose an id from the list.")
CHATS={
"zh-Hant":dict(msgs=[("user","我家的貓叫麻糬，今天又不肯吃飯"),("assistant","咦～牠是不是又挑食了"),("user","可能吧，我下班回家才發現。我是做軟體工程師的，最近都加班"),("assistant","辛苦了～"),("user","對了我下個月要去京都玩"),("assistant","哇，好羨慕！"),("user","其實我最近在吃藥治療焦慮，睡不太好"),("assistant","要多休息喔"),("user","今天天氣真好"),("assistant","是呀")],
  facts=["麻糬","軟體","京都"],sens=["焦慮","藥"],stored=[("m1","住在台中"),("m2","養了一隻貓"),("m3","喜歡喝綠茶")],corr=("不對，我現在搬到台北住了","m1")),
"zh-Hans":dict(msgs=[("user","我家的猫叫麻薯，今天又不肯吃饭"),("assistant","咦～它是不是又挑食了"),("user","可能吧，我下班回家才发现。我是做软件工程师的，最近都加班"),("assistant","辛苦了～"),("user","对了我下个月要去京都玩"),("assistant","哇，好羡慕！"),("user","其实我最近在吃药治疗焦虑，睡不太好"),("assistant","要多休息哦"),("user","今天天气真好"),("assistant","是呀")],
  facts=["麻薯","软件","京都"],sens=["焦虑","药"],stored=[("m1","住在台中"),("m2","养了一只猫"),("m3","喜欢喝绿茶")],corr=("不对，我现在搬到台北住了","m1")),
"en":dict(msgs=[("user","My cat Mochi refused to eat again today"),("assistant","Picky again?"),("user","Probably. I found out when I got home from work. I'm a software engineer and have been working overtime"),("assistant","Sounds tiring"),("user","Oh and I'm going to Kyoto next month"),("assistant","So jealous!"),("user","Actually I've been taking medication for my anxiety lately, hard to sleep"),("assistant","Get some rest"),("user","Nice weather today"),("assistant","It is!")],
  facts=["mochi","software","kyoto"],sens=["anxiety","medication"],stored=[("m1","Lives in Taichung"),("m2","Has a cat"),("m3","Likes green tea")],corr=("No, I live in Taipei now","m1")),
"ja":dict(msgs=[("user","うちの猫のモチが今日もごはんを食べないの"),("assistant","また好き嫌いかな？"),("user","たぶんね。仕事から帰って気づいた。ソフトウェアエンジニアで、最近残業が続いてる"),("assistant","お疲れさま～"),("user","そうだ、来月京都に行くんだ"),("assistant","いいなあ！"),("user","実は最近、不安症の薬を飲んでいて、あまり眠れないの"),("assistant","ゆっくり休んでね"),("user","今日はいい天気だね"),("assistant","そうだね")],
  facts=["モチ","ソフトウェア","京都"],sens=["不安","薬"],stored=[("m1","台中に住んでいる"),("m2","猫を飼っている"),("m3","緑茶が好き")],corr=("違うよ、今は台北に住んでるの","m1")),
"ko":dict(msgs=[("user","우리 고양이 모찌가 오늘도 밥을 안 먹어"),("assistant","또 편식하나 봐요"),("user","그런가 봐. 퇴근하고 와서 알았어. 나는 소프트웨어 엔지니어고 요즘 야근이 많아"),("assistant","고생이 많네요"),("user","참, 다음 달에 교토에 가"),("assistant","부럽다!"),("user","사실 요즘 불안증 약을 먹고 있어서 잠을 잘 못 자"),("assistant","푹 쉬어요"),("user","오늘 날씨 좋다"),("assistant","그러게요")],
  facts=["모찌","소프트웨어","교토"],sens=["불안","약"],stored=[("m1","타이중에 산다"),("m2","고양이를 키운다"),("m3","녹차를 좋아한다")],corr=("아니야, 지금은 타이베이에 살아","m1")),
}
def gen(system,user,temp=0.2):
    b={"model":MODEL,"stream":False,"think":False,"messages":[{"role":"system","content":system},{"role":"user","content":user}],"options":{"temperature":temp,"num_predict":800}}
    o=json.loads(subprocess.run(["curl","-s","http://localhost:11434/api/chat","-d",json.dumps(b)],capture_output=True,text=True).stdout)
    return o["message"]["content"].strip()
def parse_json(text,kind):
    m=re.search(r"\[.*\]" if kind=="list" else r"\{.*\}",text,re.S)
    if not m: return None
    try: return json.loads(m.group(0))
    except Exception: return None
def transcript(msgs): return "\n".join(f"{r}: {c}" for r,c in msgs)
tot=dict(sum_ok=0,ext_valid=0,ext_facts=0,ext_sens_leak=0,ext_runs=0,cor_ok=0)
for lang,c in CHATS.items():
    print(f"=== {lang}")
    s=gen(summary_system(lang),f"Existing notes:\n(none)\n\nNewer messages:\n{transcript(c['msgs'])}")
    has=all(fact.lower() in s.lower() for fact in c["facts"]); sleak=[w for w in c["sens"] if w.lower() in s.lower()]; tot["sum_ok"]+=has and not sleak
    print(f"  summary ({len(s)} chars, facts kept={has}, sensitive={sleak or 'none'}): {s[:140].replace(chr(10),' / ')}")
    for run in range(3):
        e=gen(EXTRACT_SYSTEM,f"Already known:\n- {c['stored'][2][1]}\n\nConversation:\n{transcript(c['msgs'])}",temp=0.3)
        arr=parse_json(e,"list"); tot["ext_runs"]+=1
        if arr is None: print(f"  extract run{run}: INVALID JSON -> {e[:100]!r}"); continue
        tot["ext_valid"]+=1; blob=json.dumps(arr,ensure_ascii=False).lower()
        found=sum(1 for f in c["facts"] if f.lower() in blob); leak=[w for w in c["sens"] if w.lower() in blob]
        tot["ext_facts"]+= found==len(c["facts"]); tot["ext_sens_leak"]+= bool(leak)
        print(f"  extract run{run}: {len(arr)} items, expected facts {found}/{len(c['facts'])}, sensitive leak={leak or 'none'}")
        if run==0: print("     ",[i.get('text') for i in arr][:6])
    new,target=c["corr"]
    cand="\n".join(f"- {i}: {t}" for i,t in c["stored"])
    r=gen(CORRECT_SYSTEM,f"New fact: {new}\nStored memories:\n{cand}",temp=0.0); j=parse_json(r,"obj")
    ok=bool(j) and j.get("replaces")==target; tot["cor_ok"]+=ok
    print(f"  correction -> {j} (correct={ok})")
print("\nTOTALS",tot)
