# Adguard External DNS
A minimal service (WrItEn In RuSt) to watch kubernetes ingress hosts and add DNS rewrites to adguard corresponding to its [status.loadBalancer.ingress[0].ip](https://kubernetes.io/docs/reference/kubernetes-api/service-resources/ingress-v1/#IngressStatus)


# Deployment
with kustomization you can do the following
```yaml
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization

bases:
  - github.com/5cat/adguard-external-dns/deploy/kustomize?ref=master  # you can pin to a commit if you want to

images:
  - name: ghcr.io/5cat/adguard-external-dns
    newTag: master  # you can pin to a commit if you want to

patches:
  - target:
      kind: Deployment
      name: adguard-external-dns
    patch: |-
      - op: replace
        path: /spec/template/spec/containers/0/env/0/value
        value: adguard.adguard.svc.local.:3000  # replace it with your adguard service url
      - op: replace
        path: /spec/template/spec/containers/0/env/1/value
        value: '.*\.example\.com'  # replace it with your domain regex to filter hosts
```
or you can download the manifests in `deploy/kustomize` and go from there
