"""Правила рекомендации: нарушение контракта не компенсируется скоростью."""
from collections import Counter


def compare(plan, cases, results, judgments, left, right, calibrated, stale):
    attempts = {(r["case"], r["repeat"], r["variant"]): r for r in results}
    assessment = {(j["case"], j["repeat"]): j for j in judgments
                  if j.get("comparator") == left and j.get("candidate") == right}
    regressions, gains, wins, losses = [], [], [], []
    incomplete = False
    unknown = False
    unresolved = False
    for case in plan["cases"]:
        for repeat in range(plan["repetitions"]):
            a, b = (attempts[case, repeat, variant] for variant in (left, right))
            key = f"{case}/{repeat + 1}"
            before, after = a.get("check", {}).get("status"), b.get("check", {}).get("status")
            unresolved = unresolved or after == "fail"
            if a["status"] != "completed" or b["status"] != "completed" or before not in {"pass", "fail"} or after not in {"pass", "fail"}:
                incomplete = True
            if before == "pass" and after == "fail": regressions.append(key)
            if before == "fail" and after == "pass": gains.append(key)
            judge = assessment.get((case, repeat), {})
            if not calibrated or judge.get("status") != "completed" or not judge.get("fresh"):
                unknown = True
                continue
            verdict, labels = judge["verdict"], judge["labels"]
            al, bl = (next(label for label, variant in labels.items() if variant == v) for v in (left, right))
            for criterion in verdict["criteria"]:
                if criterion[al] == "pass" and criterion[bl] == "fail": regressions.append(key)
                if criterion[bl] == "fail": unresolved = True
                if criterion[bl] == "unknown": unknown = True
            winner = labels.get(verdict["winner"])
            if winner == right: wins.append(case)
            if winner == left: losses.append(case)
            if verdict["winner"] == "unknown": unknown = True
    repeated = {c for c, count in Counter(wins).items() if count > plan["repetitions"] / 2}
    holdout_win = any(cases[c]["split"] == "holdout" for c in repeated)
    groups = {cases[c]["split"] for c in plan["cases"] if c in cases}
    coverage = plan["repetitions"] >= 3 and {"holdout", "control"} <= groups
    if stale:
        decision, reason = "inconclusive", "Входы или артефакты изменились"
    elif regressions:
        decision, reason = "revise", "Нарушен проверяемый контракт"
    elif unresolved:
        decision, reason = "revise", "Не пройдена обязательная проверка результата"
    elif incomplete or unknown or not calibrated or not coverage:
        decision, reason = "inconclusive", "Неполные прогоны, оценка, калибровка или покрытие"
    elif holdout_win and len(repeated) >= 2 and not losses:
        decision, reason = "accept", "Повторяемый выигрыш в проверенной области без обнаруженной регрессии"
    elif not gains and not wins:
        decision, reason = "reject", "Преимущество изменения в этом эксперименте не обнаружено"
    else:
        decision, reason = "inconclusive", "Выигрыш недостаточно повторяется либо оценки расходятся"
    return {"comparator": left, "variant": right, "decision": decision, "reason": reason,
            "regressions": sorted(set(regressions)), "improvements": gains,
            "quality_wins": len(wins), "quality_losses": len(losses), "repeated_wins": sorted(repeated)}
