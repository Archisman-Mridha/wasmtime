"""
Tools to elaborate a given Rtl with concrete types into its semantically
equivalent primitive version. Its elaborated primitive version contains only
primitive cretonne instructions, which map well to SMTLIB functions.
"""
from .primitives import GROUP as PRIMITIVES, prim_to_bv, prim_from_bv
from cdsl.xform import Rtl
from cdsl.ast import Var

try:
    from typing import TYPE_CHECKING, Dict, Union, List, Set, Tuple # noqa
    from cdsl.xform import XForm # noqa
    from cdsl.ast import Def, VarMap # noqa
    from cdsl.ti import VarTyping # noqa
except ImportError:
    TYPE_CHECKING = False


def find_matching_xform(d):
    # type: (Def) -> XForm
    """
    Given a concrete Def d, find the unique semantic XForm x in
    d.expr.inst.semantics that applies to it.
    """
    res = []  # type: List[XForm]
    typing = {v:   v.get_typevar() for v in d.vars()}  # type: VarTyping

    for x in d.expr.inst.semantics:
        subst = d.substitution(x.src.rtl[0], {})

        # There may not be a substitution if there are concrete Enumerator
        # values in the src pattern. (e.g. specifying the semantics of icmp.eq,
        # icmp.ge... as separate transforms)
        if (subst is None):
            continue

        if x.ti.permits({subst[v]: tv for (v, tv) in typing.items()}):
            res.append(x)

    assert len(res) == 1, "Couldn't find semantic transform for {}".format(d)
    return res[0]


def cleanup_semantics(r, outputs):
    # type: (Rtl, Set[Var]) -> Rtl
    """
    The elaboration process creates a lot of redundant instruction pairs of the
    shape:

        a.0 << prim_from_bv(bva.0)
        ...
        bva.1 << prim_to_bv(a.0)
        ...

    Contract these to ease manual inspection.
    """
    new_defs = []  # type: List[Def]
    subst_m = {v: v for v in r.vars()}  # type: VarMap
    definition = {}  # type: Dict[Var, Def]

    # Pass 1: Remove redundant prim_to_bv
    for d in r.rtl:
        inst = d.expr.inst

        if (inst == prim_to_bv):
            if d.expr.args[0] in definition:
                assert isinstance(d.expr.args[0], Var)
                def_loc = definition[d.expr.args[0]]

                if def_loc.expr.inst == prim_from_bv:
                    assert isinstance(def_loc.expr.args[0], Var)
                    subst_m[d.defs[0]] = def_loc.expr.args[0]
                    continue

        new_def = d.copy(subst_m)

        for v in new_def.defs:
            assert v not in definition  # Guaranteed by SSA
            definition[v] = new_def

        new_defs.append(new_def)

    # Pass 2: Remove dead prim_from_bv
    live = set(outputs)  # type: Set[Var]
    for d in new_defs:
        live = live.union(d.uses())

    new_defs = [d for d in new_defs if not (d.expr.inst == prim_from_bv and
                                            d.defs[0] not in live)]

    return Rtl(*new_defs)


def elaborate(r):
    # type: (Rtl) -> Rtl
    """
    Given a concrete Rtl r, return a semantically equivalent Rtl r1 containing
    only primitive instructions.
    """
    fp = False
    primitives = set(PRIMITIVES.instructions)
    idx = 0

    res = Rtl(*r.rtl)
    outputs = res.definitions()

    while not fp:
        assert res.is_concrete()
        new_defs = []  # type: List[Def]
        fp = True

        for d in res.rtl:
            inst = d.expr.inst

            if (inst not in primitives):
                t = find_matching_xform(d)
                transformed = t.apply(Rtl(d), str(idx))
                idx += 1
                new_defs.extend(transformed.rtl)
                fp = False
            else:
                new_defs.append(d)

        res.rtl = tuple(new_defs)

    return cleanup_semantics(res, outputs)
