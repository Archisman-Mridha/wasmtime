;;! target = "x86_64"

(module
    (func (param i64) (result f64)
        (local.get 0)
        (f64.convert_i64_s)
    )
)
;;    0:	 55                   	push	rbp
;;    1:	 4889e5               	mov	rbp, rsp
;;    4:	 4883ec10             	sub	rsp, 0x10
;;    8:	 48897c2408           	mov	qword ptr [rsp + 8], rdi
;;    d:	 4c893424             	mov	qword ptr [rsp], r14
;;   11:	 488b442408           	mov	rax, qword ptr [rsp + 8]
;;   16:	 f2480f2ac0           	cvtsi2sd	xmm0, rax
;;   1b:	 4883c410             	add	rsp, 0x10
;;   1f:	 5d                   	pop	rbp
;;   20:	 c3                   	ret	
