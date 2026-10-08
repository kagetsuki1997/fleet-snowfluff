# Regenerates the recall fixtures from the data below (task 1.1). The JSON files are the source of truth once checked in.
# Generates the recall fixtures. Fact ids m1..m30 are the same concept in every language.
import json,os
import os
OUT=os.path.join(os.path.dirname(os.path.abspath(__file__)),"..","..","..","crates","fleet-snowfluff-ai","tests","fixtures","memory")
TAGS={1:["green tea","tea"],2:["cat","pet","mochi"],3:["job","software engineer"],4:["home","taichung"],5:["allergy","peanuts"],6:["birthday"],7:["exam","toeic"],8:["guitar","music"],9:["cilantro","food dislike"],10:["bike","commute"],11:["zelda","game"],12:["japanese","language"],13:["sister","wedding"],14:["sleep","late night"],15:["thunder","fear"],16:["moving","apartment"],17:["ramen","food"],18:["running","exercise"],19:["macbook","laptop"],20:["dark mode","settings"],21:["parents","tainan"],22:["trip","kyoto"],23:["novel","reading"],24:["coffee","morning"],25:["swimming","learn"],26:["presentation","work"],27:["dentist","appointment"],28:["spicy","food dislike"],29:["postcards","collection"],30:["stress","deadline"]}
# query kinds: lexical, latin_in_cjk, one_char, english_tag, inflection, paraphrase, negative
L={}
L["zh-Hant"]=dict(
 mem=["喜歡喝無糖綠茶","養了一隻叫麻糬的貓","工作是軟體工程師","住在台中","對花生過敏","生日是三月十四日","下個月要考 TOEIC 多益","週末會彈吉他","不喜歡吃香菜","通勤都騎腳踏車","最愛的遊戲是 Zelda","正在學日文","姊姊六月要結婚","常常熬夜到凌晨兩點才睡","很怕打雷閃電","上個月剛搬家","最喜歡吃拉麵","每天早上跑五公里","工作用的是 MacBook","偏好深色模式","爸媽住在台南","打算秋天去京都旅行","在看一本推理小說","只有早上才喝咖啡","想學游泳","星期五要做簡報","下週二要去看牙醫","不敢吃辣","收集明信片","專案死線快到了壓力很大"],
 q=[("今天想喝綠茶",[1],"lexical"),("麻糬今天都不吃飯",[2],"lexical"),("你還記得我的工作是什麼嗎",[3],"lexical"),("這家餐廳有花生耶",[5],"lexical"),("我的生日快到了",[6],"lexical"),("週末想去彈吉他",[8],"lexical"),("這道菜有香菜",[9],"lexical"),("明天騎腳踏車去上班",[10],"lexical"),("日文的敬語好難",[12],"lexical"),("姊姊的婚禮要準備什麼",[13],"lexical"),("搬家後還沒整理完",[16],"lexical"),("午餐吃拉麵好嗎",[17],"lexical"),("今天早上去跑步",[18],"lexical"),("要不要回台南看爸媽",[21],"lexical"),("早上來杯咖啡",[24],"lexical"),("這個太辣了我吃不了",[28],"lexical"),
  ("新的 MacBook 好貴",[19],"latin_in_cjk"),("Zelda 出新作了",[11],"latin_in_cjk"),("TOEIC 要怎麼準備",[7],"latin_in_cjk"),
  ("貓",[2],"one_char"),("茶",[1],"one_char"),
  ("kyoto trip planning",[22],"english_tag"),("dentist",[27],"english_tag"),("novel",[23],"english_tag"),
  ("外面一直響雷好吵",[15],"paraphrase"),("螢幕亮度好刺眼",[20],"paraphrase"),("旅行帶回來的小東西",[29],"paraphrase"),
  ("今天天氣怎麼樣",[],"negative"),("你會唱歌嗎",[],"negative"),("說個笑話給我聽",[],"negative")])
L["zh-Hans"]=dict(
 mem=["喜欢喝无糖绿茶","养了一只叫麻薯的猫","工作是软件工程师","住在台中","对花生过敏","生日是三月十四日","下个月要考 TOEIC 多益","周末会弹吉他","不喜欢吃香菜","通勤都骑自行车","最爱的游戏是 Zelda","正在学日语","姐姐六月要结婚","经常熬夜到凌晨两点才睡","很怕打雷闪电","上个月刚搬家","最喜欢吃拉面","每天早上跑五公里","工作用的是 MacBook","偏好深色模式","爸妈住在台南","打算秋天去京都旅行","在看一本推理小说","只有早上才喝咖啡","想学游泳","星期五要做演示","下周二要去看牙医","不敢吃辣","收集明信片","项目截止日快到了压力很大"],
 q=[("今天想喝绿茶",[1],"lexical"),("麻薯今天都不吃饭",[2],"lexical"),("你还记得我的工作是什么吗",[3],"lexical"),("这家餐厅有花生哎",[5],"lexical"),("我的生日快到了",[6],"lexical"),("周末想去弹吉他",[8],"lexical"),("这道菜有香菜",[9],"lexical"),("明天骑自行车去上班",[10],"lexical"),("日语的敬语好难",[12],"lexical"),("姐姐的婚礼要准备什么",[13],"lexical"),("搬家后还没整理完",[16],"lexical"),("午餐吃拉面好吗",[17],"lexical"),("今天早上去跑步",[18],"lexical"),("要不要回台南看爸妈",[21],"lexical"),("早上来杯咖啡",[24],"lexical"),("这个太辣了我吃不了",[28],"lexical"),
  ("新的 MacBook 好贵",[19],"latin_in_cjk"),("Zelda 出新作了",[11],"latin_in_cjk"),("TOEIC 要怎么准备",[7],"latin_in_cjk"),
  ("猫",[2],"one_char"),("茶",[1],"one_char"),
  ("kyoto trip planning",[22],"english_tag"),("dentist",[27],"english_tag"),("novel",[23],"english_tag"),
  ("外面一直响雷好吵",[15],"paraphrase"),("屏幕亮度好刺眼",[20],"paraphrase"),("旅行带回来的小东西",[29],"paraphrase"),
  ("今天天气怎么样",[],"negative"),("你会唱歌吗",[],"negative"),("说个笑话给我听",[],"negative")])
L["en"]=dict(
 mem=["Likes unsweetened green tea","Has a cat named Mochi","Works as a software engineer","Lives in Taichung","Allergic to peanuts","Birthday is March 14","Taking the TOEIC exam next month","Plays guitar on weekends","Dislikes cilantro","Commutes by bike","Favorite game is Zelda","Is learning Japanese","Sister is getting married in June","Usually stays up until 2 am","Afraid of thunderstorms","Moved apartments last month","Favorite food is ramen","Runs five kilometers every morning","Uses a MacBook for work","Prefers dark mode","Parents live in Tainan","Planning a trip to Kyoto in autumn","Reading a mystery novel","Only drinks coffee in the morning","Wants to learn swimming","Has a presentation on Friday","Dentist appointment next Tuesday","Cannot eat spicy food","Collects postcards","Stressed about the project deadline"],
 q=[("want some green tea today",[1],"lexical"),("Mochi is not eating again",[2],"lexical"),("do you remember what my job is",[3],"paraphrase"),("this dish has peanuts in it",[5],"lexical"),("my birthday is coming up",[6],"lexical"),("feel like playing guitar this weekend",[8],"lexical"),("there is cilantro in this",[9],"lexical"),("I will bike to work tomorrow",[10],"lexical"),("Japanese keigo is hard",[12],"lexical"),("what should I prepare for the wedding",[13],"paraphrase"),("still unpacking after the move",[16],"lexical"),("ramen for lunch?",[17],"lexical"),("going for a run this morning",[18],"lexical"),("should I visit my parents in Tainan",[21],"lexical"),("morning coffee please",[24],"lexical"),("this is way too spicy for me",[28],"lexical"),
  ("MacBook prices are crazy",[19],"lexical"),("new Zelda game announced",[11],"lexical"),("how do I prepare for TOEIC",[7],"lexical"),
  ("cats are so playful",[2],"inflection"),("I studied the postcard collection today",[29],"inflection"),
  ("kyoto in the fall",[22],"lexical"),("I need to see the dentist",[27],"lexical"),("which novel next",[23],"lexical"),
  ("it keeps raining and the sky is rumbling",[15],"paraphrase"),("the screen is too bright at night",[20],"paraphrase"),("souvenirs from my travels",[29],"paraphrase"),
  ("how is the weather today",[],"negative"),("can you sing",[],"negative"),("tell me a joke",[],"negative")])
L["ja"]=dict(
 mem=["無糖の緑茶が好き","モチという名前の猫を飼っている","ソフトウェアエンジニアとして働いている","台中に住んでいる","ピーナッツアレルギーがある","誕生日は三月十四日","来月TOEICを受ける","週末はギターを弾く","パクチーが苦手","自転車で通勤している","一番好きなゲームはZelda","日本語ではなく韓国語…を勉強中","姉が六月に結婚する","夜更かしして午前二時ごろ寝ることが多い","雷が怖い","先月引っ越した","一番好きな食べ物はラーメン","毎朝五キロ走っている","仕事ではMacBookを使っている","ダークモードが好み","両親は台南に住んでいる","秋に京都へ旅行する予定","推理小説を読んでいる","コーヒーは朝しか飲まない","水泳を習いたい","金曜日にプレゼンがある","来週の火曜日に歯医者の予約がある","辛いものは食べられない","絵はがきを集めている","プロジェクトの締め切りが近くてストレスがたまっている"],
 q=[("今日は緑茶が飲みたい",[1],"lexical"),("モチがごはんを食べない",[2],"lexical"),("私の仕事を覚えている？",[3],"paraphrase"),("この料理にピーナッツが入っている",[5],"lexical"),("もうすぐ誕生日だ",[6],"lexical"),("週末にギターを弾きたい",[8],"lexical"),("これにパクチーが入ってる",[9],"lexical"),("明日は自転車で出勤する",[10],"lexical"),("敬語がむずかしい",[12],"paraphrase"),("姉の結婚式の準備",[13],"lexical"),("引っ越しの片付けがまだ終わらない",[16],"lexical"),("お昼はラーメンにしよう",[17],"lexical"),("今朝はジョギングに行った",[18],"paraphrase"),("実家の両親に会いに行こうかな",[21],"paraphrase"),("朝のコーヒーが欲しい",[24],"lexical"),("これは辛すぎて食べられない",[28],"lexical"),
  ("新しいMacBookは高い",[19],"latin_in_cjk"),("Zeldaの新作が出た",[11],"latin_in_cjk"),("TOEICの勉強法は？",[7],"latin_in_cjk"),
  ("猫",[2],"one_char"),("雷",[15],"one_char"),
  ("kyoto trip planning",[22],"english_tag"),("dentist",[27],"english_tag"),("novel",[23],"english_tag"),
  ("外が暗くてゴロゴロ鳴っている",[15],"paraphrase"),("画面がまぶしい",[20],"paraphrase"),("旅行のお土産",[29],"paraphrase"),
  ("今日の天気はどう",[],"negative"),("歌える？",[],"negative"),("冗談を言って",[],"negative")])
L["ko"]=dict(
 mem=["무가당 녹차를 좋아한다","모찌라는 이름의 고양이를 키운다","소프트웨어 엔지니어로 일한다","타이중에 산다","땅콩 알레르기가 있다","생일은 3월 14일이다","다음 달에 TOEIC 시험을 본다","주말에는 기타를 친다","고수를 싫어한다","자전거로 출퇴근한다","가장 좋아하는 게임은 Zelda다","일본어를 공부하고 있다","누나가 6월에 결혼한다","보통 새벽 두 시까지 안 자고 있다","천둥을 무서워한다","지난달에 이사했다","가장 좋아하는 음식은 라멘이다","매일 아침 5킬로미터를 뛴다","일할 때 MacBook을 쓴다","다크 모드를 선호한다","부모님은 타이난에 사신다","가을에 교토로 여행갈 계획이다","추리 소설을 읽고 있다","커피는 아침에만 마신다","수영을 배우고 싶어 한다","금요일에 발표가 있다","다음 주 화요일에 치과 예약이 있다","매운 음식을 못 먹는다","엽서를 모은다","프로젝트 마감이 다가와서 스트레스를 받는다"],
 q=[("오늘은 녹차가 마시고 싶다",[1],"lexical"),("모찌가 밥을 안 먹어",[2],"lexical"),("내 직업이 뭔지 기억해?",[3],"paraphrase"),("이 음식에 땅콩이 들어 있어",[5],"lexical"),("곧 내 생일이야",[6],"lexical"),("주말에 기타 치고 싶다",[8],"lexical"),("여기에 고수가 들어 있네",[9],"lexical"),("내일은 자전거로 출근할 거야",[10],"lexical"),("일본어 존댓말이 어려워",[12],"lexical"),("누나 결혼식 준비는 어떻게 하지",[13],"lexical"),("이사하고 아직 정리를 못 했어",[16],"lexical"),("점심은 라멘 어때",[17],"lexical"),("오늘 아침에 달리기를 했어",[18],"paraphrase"),("부모님 뵈러 타이난에 갈까",[21],"lexical"),("아침 커피 한 잔 줘",[24],"lexical"),("이건 너무 매워서 못 먹겠어",[28],"lexical"),
  ("새 MacBook이 너무 비싸",[19],"latin_in_cjk"),("Zelda 신작이 나왔대",[11],"latin_in_cjk"),("TOEIC 준비는 어떻게 해",[7],"latin_in_cjk"),
  ("고양이",[2],"lexical"),("차",[1],"one_char"),
  ("kyoto trip planning",[22],"english_tag"),("dentist",[27],"english_tag"),("novel",[23],"english_tag"),
  ("밖에서 우르릉 소리가 계속 나",[15],"paraphrase"),("화면이 너무 밝아",[20],"paraphrase"),("여행에서 사 온 기념품",[29],"paraphrase"),
  ("오늘 날씨 어때",[],"negative"),("노래 할 수 있어?",[],"negative"),("농담 하나 해줘",[],"negative")])
# japanese fact 12: fix wording so it is a coherent memory
L["ja"]["mem"][11]="日本語の敬語を勉強中"
L["ja"]["q"][8]=("敬語がむずかしい",[12],"lexical")
for lang,d in L.items():
    mem=[{"id":f"m{i+1}","text":t,"tags":([] if lang=="en" else TAGS[i+1])} for i,t in enumerate(d["mem"])]
    qs=[{"id":f"q{i+1}","text":t,"expect":[f"m{e}" for e in ex],"kind":k} for i,(t,ex,k) in enumerate(d["q"])]
    assert len(mem)==30 and len(qs)==30,(lang,len(mem),len(qs))
    json.dump({"language":lang,"memories":mem,"queries":qs},open(f"{OUT}/recall_{lang}.json","w"),ensure_ascii=False,indent=1)
    print(lang,len(mem),len(qs))
