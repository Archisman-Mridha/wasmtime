;;! target = "x86_64"

(module
    (func (param f64) (result i32)
        (local.get 0)
        (i32.trunc_f64_s)
    )
)
;;    0:	 55                   	push	rbp
;;    1:	 4889e5               	mov	rbp, rsp
;;    4:	 4883ec10             	sub	rsp, 0x10
;;    8:	 f20f11442408         	movsd	qword ptr [rsp + 8], xmm0
;;    e:	 4c893424             	mov	qword ptr [rsp], r14
;;   12:	 f20f10442408         	movsd	xmm0, qword ptr [rsp + 8]
;;   18:	 f20f2cc0             	cvttsd2si	eax, xmm0
;;   1c:	 83f801               	cmp	eax, 1
;;   1f:	 0f8134000000         	jno	0x59
;;   25:	 660f2ec0             	ucomisd	xmm0, xmm0
;;   29:	 0f8a30000000         	jp	0x5f
;;   2f:	 49bb000020000000e0c1 	
;; 				movabs	r11, 0xc1e0000000200000
;;   39:	 664d0f6efb           	movq	xmm15, r11
;;   3e:	 66410f2ec7           	ucomisd	xmm0, xmm15
;;   43:	 0f8618000000         	jbe	0x61
;;   49:	 66450f57ff           	xorpd	xmm15, xmm15
;;   4e:	 66440f2ef8           	ucomisd	xmm15, xmm0
;;   53:	 0f820a000000         	jb	0x63
;;   59:	 4883c410             	add	rsp, 0x10
;;   5d:	 5d                   	pop	rbp
;;   5e:	 c3                   	ret	
;;   5f:	 0f0b                 	ud2	
;;   61:	 0f0b                 	ud2	
;;   63:	 0f0b                 	ud2	
