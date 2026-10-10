"""Strict, opt-in native Turso/Tantivy search behavior probe on temporary files."""
from __future__ import annotations

import gc
import json
import math
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

import turso
from turso import _turso


def rows(connection, sql, parameters=()):
    cursor = connection.execute(sql, parameters)
    try:
        return cursor.fetchall()
    finally:
        cursor.close()


def connect(path):
    connection = turso.connect(str(path), isolation_level=None,
                               experimental_features="index_method,generated_columns")
    assert rows(connection, "PRAGMA journal_mode=mvcc") == [("mvcc",)]
    return connection


def exploratory(path):
    connection = connect(path)
    rows(connection, "CREATE TABLE docs(id INTEGER PRIMARY KEY, content TEXT NOT NULL)")
    rows(connection, "CREATE INDEX docs_search ON docs USING fts(content)")
    rows(connection, "INSERT INTO docs VALUES (1,'alpha beta'),(2,'alpha alpha beta'),(3,'alpha gamma')")
    queries = ["alpha", '"alpha beta"', "alpha AND beta", "alpha NOT gamma", '"alpha-beta"', '"a\\\"b"', 'alp*', '"alp"*', '" "', '"!!!"', '"🚨 :: --"', '""']
    result = {}
    for query in queries:
        sql = "SELECT id, fts_score(content, ?1) FROM docs WHERE fts_match(content, ?1) ORDER BY fts_score(content, ?1) DESC, id"
        try:
            result[query] = {"rows": rows(connection, sql, (query,)),
                             "plan": rows(connection, "EXPLAIN QUERY PLAN " + sql, (query,))}
        except turso.DatabaseError as error:
            result[query] = {"error": str(error)}
    result["callbacks"] = {name: hasattr(connection, name) for name in
                           ("set_authorizer", "set_progress_handler", "set_update_hook", "interrupt", "set_query_timeout")}
    for order in ("score DESC", "-score ASC", "fts_score(content, ?1) DESC", "2 DESC"):
        result["order:" + order] = rows(connection, "SELECT id, fts_score(content, ?1) AS score FROM docs WHERE fts_match(content, ?1) ORDER BY " + order + ", id", ("alpha",))
    for sql in (
        "WITH hits AS MATERIALIZED (SELECT id,fts_score(content,?1) AS score FROM docs WHERE fts_match(content,?1)) SELECT id,score FROM hits ORDER BY score DESC,id",
        "SELECT id, score FROM (SELECT id,fts_score(content,?1) AS score FROM docs WHERE fts_match(content,?1) LIMIT 1000000) ORDER BY score DESC,id",
        "SELECT id,fts_score(content,?1) AS score FROM docs WHERE fts_match(content,?1) ORDER BY score DESC,id LIMIT 2",
    ):
        try:
            result[sql] = rows(connection, sql, ("alpha",))
        except turso.DatabaseError as error:
            result[sql] = {"error":str(error)}
    for query in ('"alpha b"*', 'alpha*', '"alpha"', 'alpha OR gamma'):
        result[query] = rows(connection,"SELECT id FROM docs WHERE fts_match(content,?1) ORDER BY id",(query,))
    rows(connection,"CREATE TABLE members(evidence_id INTEGER NOT NULL,doc_id INTEGER NOT NULL, PRIMARY KEY(evidence_id,doc_id))")
    rows(connection,"INSERT INTO members VALUES(1,1),(2,3)")
    for sql in (
        "SELECT id FROM docs WHERE id=?2 AND fts_match(content,?1)",
        "SELECT evidence_id FROM members WHERE EXISTS(SELECT 1 FROM docs WHERE docs.id=members.doc_id AND fts_match(docs.content,?1)) ORDER BY evidence_id",
        "SELECT evidence_id FROM members JOIN docs ON docs.id=members.doc_id WHERE fts_match(docs.content,?1) ORDER BY evidence_id",
    ):
        parameters=('alpha NOT gamma',3) if '?2' in sql else ('alpha NOT gamma',)
        result['constrained:'+sql]={"rows":rows(connection,sql,parameters),"plan":rows(connection,"EXPLAIN QUERY PLAN "+sql,parameters)}
        assert result['constrained:'+sql]["rows"]==([] if '?2' in sql else [(1,)]),(sql,result['constrained:'+sql])
        assert any("QUERY INDEX METHOD fts" in row[-1] for row in result['constrained:'+sql]["plan"]),(sql,result['constrained:'+sql])
    rows(connection, "CREATE TABLE multi(id INTEGER PRIMARY KEY,index_text TEXT,role TEXT,kind TEXT,model TEXT,tool_names TEXT)")
    rows(connection, "CREATE INDEX multi_search ON multi USING fts(index_text,role,kind,model,tool_names) WITH(weights='index_text=10.0,role=2.0,kind=1.0,model=1.0,tool_names=1.0')")
    rows(connection, "INSERT INTO multi VALUES (1,'alpha beta','user','raw','',''),(2,'none','alpha','raw','','')")
    for cols, query in (("index_text", "alpha"), ("index_text,role,kind,model,tool_names", 'index_text:("alpha" AND "beta")'), ("index_text,role,kind,model,tool_names", 'index_text:alp*')):
        sql = "SELECT id FROM multi WHERE fts_match(" + cols + ",?1) ORDER BY id"
        try:
            result["multi:" + cols + ":" + query] = {"rows":rows(connection,sql,(query,)),"plan":rows(connection,"EXPLAIN QUERY PLAN "+sql,(query,))}
        except turso.DatabaseError as error:
            result["multi:" + cols + ":" + query] = {"error":str(error)}
    assert [row[0] for row in result['"alpha beta"']["rows"]]==[1,2]
    assert [row[0] for row in result["alpha AND beta"]["rows"]]==[1,2]
    assert [row[0] for row in result["alpha NOT gamma"]["rows"]]==[1,2]
    assert result["alp*"]["rows"]==[]
    for query in ('" "', '"!!!"', '"🚨 :: --"', '""'):
        assert result[query]["rows"]==[],(query,result[query])
        assert any("QUERY INDEX METHOD fts" in row[-1] for row in result[query]["plan"]),(query,result[query])
    assert "PhrasePrefixRequiresAtLeastTwoTerms" in result['"alp"*']["error"]
    assert result['"alpha b"*']==[(1,),(2,)]
    assert result["multi:index_text:alpha"]["rows"]==[(1,)]
    assert any("SCAN multi" in row[-1] for row in result["multi:index_text:alpha"]["plan"])
    assert any("QUERY INDEX METHOD fts" in row[-1] for row in result['multi:index_text,role,kind,model,tool_names:index_text:("alpha" AND "beta")']["plan"])
    assert result["order:score DESC"][1][1]>result["order:score DESC"][0][1]
    materialized=next(value for key,value in result.items() if key.startswith("WITH hits AS MATERIALIZED"))
    assert [row[0] for row in materialized]==[2,1,3] and materialized[0][1]>materialized[1][1]
    connection.close()
    del connection
    gc.collect()
    return result


def lifecycle(path):
    connection = connect(path)
    rows(connection,"CREATE TABLE docs(id INTEGER PRIMARY KEY,content TEXT NOT NULL)")
    rows(connection,"CREATE INDEX docs_search ON docs USING fts(content)")
    rows(connection,"INSERT INTO docs VALUES(1,'beforeword'),(2,'keepword')")
    reader = connect(path)
    rows(reader,"BEGIN CONCURRENT")
    before = rows(reader,"SELECT id FROM docs WHERE fts_match(content,'beforeword')")
    rows(connection,"BEGIN CONCURRENT")
    rows(connection,"UPDATE docs SET content='afterword' WHERE id=1")
    own_uncommitted = rows(connection,"SELECT id FROM docs WHERE fts_match(content,'afterword')")
    rows(connection,"COMMIT")
    pinned_after_update = rows(reader,"SELECT id FROM docs WHERE fts_match(content,'beforeword')")
    pinned_sql_row = rows(reader,"SELECT content FROM docs WHERE id=1")
    fresh_after_update = rows(connection,"SELECT id FROM docs WHERE fts_match(content,'afterword')")
    rows(reader,"ROLLBACK")
    rows(connection,"BEGIN CONCURRENT")
    rows(connection,"INSERT INTO docs VALUES(3,'rollbackword')")
    rows(connection,"DELETE FROM docs WHERE id=2")
    rows(connection,"ROLLBACK")
    after_rollback = rows(connection,"SELECT id FROM docs WHERE fts_match(content,'rollbackword OR keepword')")
    rows(reader,"BEGIN CONCURRENT")
    rows(reader,"SELECT id FROM docs WHERE fts_match(content,'afterword')")
    rows(connection,"DELETE FROM docs WHERE id=1")
    pinned_after_delete = rows(reader,"SELECT id FROM docs WHERE fts_match(content,'afterword')")
    pinned_deleted_sql = rows(reader,"SELECT content FROM docs WHERE id=1")
    rows(reader,"ROLLBACK")
    connection.close()
    reader.close()
    del connection,reader
    gc.collect()
    result={"before":before,"own_uncommitted":own_uncommitted,
            "pinned_after_update":pinned_after_update,"pinned_sql_row":pinned_sql_row,
            "fresh_after_update":fresh_after_update,"after_rollback":after_rollback,
            "pinned_after_delete":pinned_after_delete,"pinned_deleted_sql":pinned_deleted_sql}
    assert result=={"before":[(1,)],"own_uncommitted":[(1,)],"pinned_after_update":[(1,)],
                    "pinned_sql_row":[("beforeword",)],"fresh_after_update":[(1,)],
                    "after_rollback":[(2,)],"pinned_after_delete":[(1,)],
                    "pinned_deleted_sql":[("afterword",)]},result
    return result


def same_index_overlap(path):
    left=connect(path)
    rows(left,"CREATE TABLE docs(id INTEGER PRIMARY KEY,content TEXT NOT NULL)")
    rows(left,"CREATE INDEX docs_search ON docs USING fts(content)")
    rows(left,"INSERT INTO docs VALUES(1,'beforeone'),(2,'beforetwo')")
    right=connect(path)
    rows(left,"BEGIN CONCURRENT")
    rows(right,"BEGIN CONCURRENT")
    rows(left,"UPDATE docs SET content='afterone' WHERE id=1")
    rows(right,"UPDATE docs SET content='aftertwo' WHERE id=2")
    assert left.in_transaction and right.in_transaction
    rows(left,"COMMIT")
    rows(right,"COMMIT")
    assert sorted(rows(left,"SELECT id FROM docs WHERE fts_match(content,'afterone OR aftertwo')"))==[(1,),(2,)]
    left.close()
    right.close()
    return {"overlapping_same_index_writers_committed_independent_rows":True}


def reentrant_trigger(path):
    connection = connect(path)
    rows(connection,"CREATE TABLE docs(id INTEGER PRIMARY KEY,content TEXT NOT NULL)")
    rows(connection,"CREATE INDEX docs_search ON docs USING fts(content)")
    rows(connection,"CREATE TRIGGER derive_after_insert AFTER INSERT ON docs BEGIN UPDATE docs SET content=content||' derivedword' WHERE id=NEW.id; END")
    try:
        rows(connection,"INSERT INTO docs VALUES(1,'originalword')")
    except turso.DatabaseError as error:
        message=str(error)
        assert message == "statement already has an open writer on this FTS index; a trigger cannot write the FTS-indexed table its firing statement is writing", message
    else:
        raise AssertionError("same-index trigger probe unexpectedly succeeded")
    assert rows(connection,"SELECT * FROM docs") == []
    assert rows(connection,"SELECT id FROM docs WHERE fts_match(content,'originalword OR derivedword')") == []
    connection.close()
    return {"unsupported_same_index_trigger":message,"failed_statement_left_no_rows_or_hits":True}


SPECS = {
    "memory_v2_assertion_payloads":("content",),
    "lcm_raw_messages":("index_text","role","kind","model","tool_names"),
    "session_occurrences":("index_text",),
    "session_summary_nodes":("summary_text",),
}


def quote(term):
    return '"' + term.replace('\\','\\\\').replace('"','\\"') + '"'


def insert_canonical(connection, table, identifier, text):
    if table == "memory_v2_assertion_payloads":
        data={"rowid":identifier,"assertion_id":f"a{identifier}","fact_id":"fact","owner_kind":"project","project_id":"project","payload_json":"{}","content":text}
    elif table == "lcm_raw_messages":
        data={"store_id":identifier,"provider":"provider","message_id":f"m{identifier}","session_id":"session","role":"alpha" if identifier==6 else "user","ordinal":identifier,"content":text,"content_hash":"hash","storage_kind":"inline","kind":"raw","model":"","tool_names":""}
    elif table == "session_occurrences":
        data={"rowid":identifier,"session_id":"session","generation":1,"occurrence_id":f"o{identifier}","source_observation_id":f"obs{identifier}","source_sequence":identifier,"source_provider":"provider","projection_output_ordinal":0,"retrieval_anchor_id":f"anchor{identifier}","copied_from_anchor_ids_json":"[]","role":"user","knowledge_at":identifier,"valid_time_json":'{"kind":"unknown"}',"evidence_json":"{}","sanitized_content_digest":"0"*64,"sanitized_content_bytes":len(text.encode()),"index_text":text}
    else:
        data={"rowid":identifier,"summary_id":f"sum{identifier}","session_id":"session","provider":"provider","conversation_id":"conversation","depth":0,"summary_anchor_id":f"anchor{identifier}","summary_text":text,"summary_hash":"hash","summary_token_count":1,"source_token_count":1,"source_horizon_json":"[]","publication_json":'{"provider":"provider"}',"created_at":identifier}
    rows(connection,"INSERT INTO "+table+"("+",".join(data)+") VALUES("+",".join("?" for _ in data)+")",tuple(data.values()))


def setup_canonical(connection):
    cursor=connection.executescript(Path(__file__).with_name("native_search_schema.sql").read_text())
    cursor.close()
    rows(connection,"PRAGMA foreign_keys=ON")
    rows(connection,"INSERT INTO sessions VALUES('provider','session')")
    rows(connection,"INSERT INTO session_temporal_generations VALUES('session',1)")
    for identifier in range(1,10):
        rows(connection,"INSERT INTO memory_v2_assertions VALUES(?,'fact','project','project')",(f"a{identifier}",))
        rows(connection,"INSERT INTO retrieval_anchors VALUES(?)",(f"anchor{identifier}",))
        rows(connection,"INSERT INTO observations VALUES(?)",(f"obs{identifier}",))
    for table in SPECS:
        for identifier,text in enumerate(("alpha beta","alpha alpha beta","gamma delta",'quote "gamma" slash \\delta café naïve',"alpha OR beta","metadataonly"),1):
            insert_canonical(connection,table,identifier,text)


def query_sql(table, joined=False):
    columns=",".join("t."+column for column in SPECS[table])
    if joined:
        return "WITH native_hits AS MATERIALIZED (SELECT t.rowid AS doc_id,t.retrieval_anchor_id,fts_score("+columns+",?1) AS score FROM "+table+" AS t WHERE fts_match("+columns+",?1)) SELECT h.doc_id,h.score FROM native_hits AS h JOIN retrieval_anchors AS a ON a.anchor_id=h.retrieval_anchor_id"
    return "SELECT t.rowid AS doc_id,fts_score("+columns+",?1) AS score FROM "+table+" AS t WHERE fts_match("+columns+",?1)"


def canonical(path):
    connection=connect(path)
    setup_canonical(connection)
    result={}
    plain_join="SELECT t.rowid,fts_score(t.index_text,?1) FROM session_occurrences t JOIN retrieval_anchors a ON a.anchor_id=t.retrieval_anchor_id WHERE fts_match(t.index_text,?1) ORDER BY t.rowid"
    plain_result=rows(connection,plain_join,('"alpha"',))
    plain_plan=rows(connection,"EXPLAIN QUERY PLAN "+plain_join,('"alpha"',))
    assert plain_result==[(1,0.0),(2,0.0),(5,0.0)],plain_result
    assert any("QUERY INDEX METHOD fts" in row[-1] for row in plain_plan),plain_plan
    result["direct_join_score_limitation"]={"rows":plain_result,"plan":plain_plan}
    for table,columns in SPECS.items():
        sql=query_sql(table,table=="session_occurrences")
        body=lambda query: "index_text:("+query+")" if len(columns)>1 else query
        checks={"term":(quote("alpha"),[1,2,5]),"phrase":(quote("alpha beta"),[1,2]),
                "boolean_and":('"alpha" AND "beta"',[1,2,5]),
                "boolean_not":('"alpha" NOT "beta"',[]),
                "escaped_quotes":(quote('quote "gamma"'),[4]),
                "escaped_backslash":(quote('slash \\delta'),[4]),
                "unicode":(quote("café"),[4]),
                "literal_boolean_operator":(quote("alpha OR beta"),[5])}
        observed={}
        for name,(query,expected) in checks.items():
            query=body(query)
            actual=rows(connection,sql+" ORDER BY doc_id",(query,))
            assert [row[0] for row in actual] == expected,(table,name,actual)
            plan=rows(connection,"EXPLAIN QUERY PLAN "+sql,(query,))
            assert any("QUERY INDEX METHOD fts" in row[-1] for row in plan),(table,name,plan)
            assert all(math.isfinite(row[1]) and row[1]>0 for row in actual),(table,name,actual,plan)
            observed[name]={"ids":expected,"query":query,"plan":plan}
        ranked="WITH hits AS MATERIALIZED ("+sql+") SELECT doc_id,score FROM hits ORDER BY score DESC,doc_id LIMIT 2"
        ranking=rows(connection,ranked,(body(quote("alpha")),))
        assert [row[0] for row in ranking]==[2,1] and ranking[0][1]>ranking[1][1],ranking
        observed["materialized_top_k"]=ranking
        rows(connection,"BEGIN CONCURRENT")
        insert_canonical(connection,table,7,"rollbackword")
        assert [row[0] for row in rows(connection,sql,(body(quote("rollbackword")),))]==[7]
        rows(connection,"DELETE FROM "+table+" WHERE rowid=3")
        rows(connection,"ROLLBACK")
        assert rows(connection,sql,(body(quote("rollbackword")),))==[]
        assert [row[0] for row in rows(connection,sql,(body(quote("gamma delta")),))]==[3]
        if table=="memory_v2_assertion_payloads":
            try:
                rows(connection,"UPDATE memory_v2_assertion_payloads SET content='mutated' WHERE rowid=1")
            except turso.DatabaseError as error:
                assert "memory_v2 assertion payloads are immutable" in str(error),error
            else:
                raise AssertionError("canonical immutable payload trigger was bypassed")
            immutable_hits=rows(connection,sql,(quote("alpha beta"),))
            assert sorted(row[0] for row in immutable_hits)==[1,2],immutable_hits
            observed["canonical_immutable_trigger_preserved"]=True
        else:
            field="content" if table=="lcm_raw_messages" else columns[0]
            rows(connection,"UPDATE "+table+" SET "+field+"='replacementword' WHERE rowid=3")
            assert [row[0] for row in rows(connection,sql,(body(quote("replacementword")),))]==[3]
            assert rows(connection,sql,(body(quote("gamma delta")),))==[]
        rows(connection,"DELETE FROM "+table+" WHERE rowid=3")
        assert rows(connection,sql,(body(quote("replacementword")),))==[]
        observed["rollback_and_delete_verified"]=True
        result[table]=observed
    connection.close()
    del connection
    gc.collect()
    child=subprocess.run([sys.executable,__file__,"--reopen",str(path)],capture_output=True,text=True,timeout=15,check=True)
    result["fresh_process_reopen"]=json.loads(child.stdout)
    assert math.isclose(result["lcm_raw_messages"]["materialized_top_k"][0][1],
                        10*result["memory_v2_assertion_payloads"]["materialized_top_k"][0][1],rel_tol=1e-6)
    result["higher_positive_native_bm25_is_better_and_content_weight_is_10x"]=True
    return result


def reopened(path):
    connection=connect(path)
    for table,columns in SPECS.items():
        query='index_text:("alpha")' if len(columns)>1 else '"alpha"'
        assert sorted(row[0] for row in rows(connection,query_sql(table),(query,)))==[1,2,5]
        assert rows(connection,"SELECT rowid FROM "+table+" WHERE rowid IN (3,7)")==[]
    integrity=rows(connection,"PRAGMA integrity_check")
    assert integrity==[("ok",)],integrity
    connection.close()
    return {"four_native_indexes_persisted":True,"integrity_check":integrity}


def cancellation(path):
    connection=connect(path)
    rows(connection,"CREATE TABLE docs(id INTEGER PRIMARY KEY,content TEXT NOT NULL)")
    rows(connection,"CREATE INDEX docs_search ON docs USING fts(content)")
    rows(connection,"INSERT INTO docs VALUES(1,'stableword')")
    unavailable={}
    for name in ("set_authorizer","set_progress_handler","set_update_hook"):
        try:
            getattr(connection,name)(lambda *args:0)
        except AttributeError as error:
            unavailable[name]={"class":type(error).__name__,"message":str(error)}
        else:
            raise AssertionError("hook SDK surface changed; audit replacement")
    expensive="WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT SUM(x) FROM n"
    connection.set_query_timeout(30)
    assert connection.get_query_timeout()==30
    started=time.monotonic()
    try:
        rows(connection,expensive)
    except turso.OperationalError as error:
        assert "interrupt" in str(error).lower(),error
    else:
        raise AssertionError("deadline did not cancel SQL")
    deadline_elapsed=time.monotonic()-started
    assert deadline_elapsed<3,deadline_elapsed
    rows(connection,"BEGIN CONCURRENT")
    try:
        rows(connection,"WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) INSERT INTO docs SELECT x+10,'cancelword' FROM n")
    except turso.OperationalError as error:
        assert "interrupt" in str(error).lower(),error
    else:
        raise AssertionError("deadline did not cancel indexed write")
    connection.set_query_timeout(0)
    if connection.in_transaction:
        rows(connection,"ROLLBACK")
    assert rows(connection,"SELECT * FROM docs")==[(1,"stableword")]
    assert rows(connection,"SELECT id FROM docs WHERE fts_match(content,'cancelword')")==[]
    connection.set_query_timeout(3000)
    fired=threading.Event()
    def cancel():
        time.sleep(.03)
        connection.interrupt()
        fired.set()
    watchdog=threading.Thread(target=cancel)
    watchdog.start()
    started=time.monotonic()
    try:
        rows(connection,expensive)
    except turso.OperationalError as error:
        assert "interrupt" in str(error).lower(),error
    else:
        raise AssertionError("interrupt did not cancel SQL")
    interrupted_elapsed=time.monotonic()-started
    watchdog.join(timeout=1)
    assert fired.is_set() and not watchdog.is_alive() and interrupted_elapsed<2
    connection.set_query_timeout(0)
    assert rows(connection,"SELECT id FROM docs WHERE fts_match(content,'stableword')")==[(1,)]
    connection.close()
    return {"unavailable_python_callback_apis":unavailable,"native_deadline_cancelled_seconds":deadline_elapsed,
            "cross_thread_native_interrupt_cancelled_seconds":interrupted_elapsed,"indexed_query_works_after_cancellation":True,
            "cancelled_indexed_write_rolled_back_without_rows_or_hits":True}


def secure_delete(path):
    connection=connect(path)
    before=rows(connection,"PRAGMA secure_delete")
    rows(connection,"PRAGMA secure_delete=ON")
    enabled=rows(connection,"PRAGMA secure_delete")
    assert before==[] and enabled==[],("pinned native secure_delete capability changed",before,enabled)
    connection.close()
    return {"before":before,"enabled":enabled,"logical_deletion_approved":True,"deleted_page_scrubbing":False}


if __name__ == "__main__":
    assert _turso.__version__ == "0.8.0"
    if len(sys.argv)==3 and sys.argv[1]=="--reopen":
        print(json.dumps(reopened(Path(sys.argv[2])),sort_keys=True))
        sys.exit(0)
    with tempfile.TemporaryDirectory(prefix="tracedecay-native-search-") as directory:
        if len(sys.argv)>1:
            print(json.dumps(globals()[sys.argv[1]](Path(directory)/"search.db"),sort_keys=True))
        else:
            result = {}
            for scenario in ("exploratory","lifecycle","same_index_overlap","reentrant_trigger","canonical","cancellation","secure_delete"):
                try:
                    child=subprocess.run([sys.executable,__file__,scenario],capture_output=True,text=True,timeout=20,check=False)
                    assert child.returncode==0,(scenario,child.stderr)
                    result[scenario]=json.loads(child.stdout)
                except subprocess.TimeoutExpired as error:
                    raise AssertionError((scenario,"native probe exceeded 20 seconds")) from error
            print(json.dumps(result,sort_keys=True))
